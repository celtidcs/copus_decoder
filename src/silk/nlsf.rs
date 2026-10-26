//! Reconstrucción de las NLSF de SILK.

use super::entropy_tables::NLSF_EC_SEL_PRED_SHIFTS;
use super::fixed::rshift_round;
use super::tables::{MAX_LPC_ORDER, NlsfCodebook};

const MAX_LOOPS: usize = 20;
const NLSF_QUANT_LEVEL_ADJ_Q10: i32 = 102;

/// Decodifica la cuantificación vectorial en dos etapas de las NLSF: el vector de la primera
/// etapa más los residuos predictivos de la segunda, con los índices agrupados `nlsf_indices` y
/// el libro `codebook` (de banda estrecha y media o de banda ancha). Escribe en
/// `nlsf_q15[..order]` las NLSF estabilizadas, en Q15.
pub(super) fn nlsf_decode(nlsf_q15: &mut [i16], nlsf_indices: &[i8], codebook: &NlsfCodebook) {
    let order = codebook.order;
    debug_assert!(nlsf_q15.len() >= order);
    debug_assert!(nlsf_indices.len() > order);

    let cb1_index = nlsf_indices[0] as usize;
    let mut pred_q8 = [0u8; MAX_LPC_ORDER];
    let mut res_q10 = [0i16; MAX_LPC_ORDER];
    unpack_predictors(&mut pred_q8, codebook, cb1_index);
    residual_dequant(
        &mut res_q10,
        &nlsf_indices[1..=order],
        &pred_q8,
        codebook.quant_step_size_q16,
        order,
    );

    let cb_base = cb1_index * order;
    for i in 0..order {
        let weighted_q15 = ((res_q10[i] as i32) << 14) / codebook.cb1_wght_q9[cb_base + i] as i32;
        let centroid_q15 = (codebook.cb1_nlsf_q8[cb_base + i] as i32) << 7;
        nlsf_q15[i] = (weighted_q15 + centroid_q15).clamp(0, i16::MAX as i32) as i16;
    }

    nlsf_stabilize(nlsf_q15, codebook.delta_min_q15, order);
}

/// Estabiliza en el sitio las `order` NLSF de `nlsf_q15` para que respeten las distancias
/// mínimas `delta_min_q15` del libro (`order + 1` entradas), entre sí y con los extremos. Si
/// tras `MAX_LOOPS` pasadas no lo consigue, ordena las NLSF y las fuerza a esas distancias.
pub(super) fn nlsf_stabilize(nlsf_q15: &mut [i16], delta_min_q15: &[i16], order: usize) {
    debug_assert!(nlsf_q15.len() >= order);

    for _ in 0..MAX_LOOPS {
        let mut min_diff_q15 = nlsf_q15[0] as i32 - delta_min_q15[0] as i32;
        let mut split = 0usize;

        for i in 1..order {
            let diff_q15 = nlsf_q15[i] as i32 - (nlsf_q15[i - 1] as i32 + delta_min_q15[i] as i32);
            if diff_q15 < min_diff_q15 {
                min_diff_q15 = diff_q15;
                split = i;
            }
        }

        let tail_diff_q15 = (1 << 15) - (nlsf_q15[order - 1] as i32 + delta_min_q15[order] as i32);
        if tail_diff_q15 < min_diff_q15 {
            min_diff_q15 = tail_diff_q15;
            split = order;
        }

        if min_diff_q15 >= 0 {
            return;
        }

        if split == 0 {
            nlsf_q15[0] = delta_min_q15[0];
        } else if split == order {
            nlsf_q15[order - 1] = ((1 << 15) - delta_min_q15[order] as i32) as i16;
        } else {
            let mut min_center_q15 = 0i32;
            for &delta in &delta_min_q15[..split] {
                min_center_q15 += delta as i32;
            }
            min_center_q15 += (delta_min_q15[split] as i32) >> 1;

            let mut max_center_q15 = 1 << 15;
            for &delta in &delta_min_q15[split + 1..=order] {
                max_center_q15 -= delta as i32;
            }
            max_center_q15 -= (delta_min_q15[split] as i32) >> 1;

            let center_q15 = limit32(
                rshift_round(nlsf_q15[split - 1] as i32 + nlsf_q15[split] as i32, 1),
                min_center_q15,
                max_center_q15,
            );
            nlsf_q15[split - 1] = (center_q15 - ((delta_min_q15[split] as i32) >> 1)) as i16;
            nlsf_q15[split] = (nlsf_q15[split - 1] as i32 + delta_min_q15[split] as i32) as i16;
        }
    }

    nlsf_q15[..order].sort_unstable();
    nlsf_q15[0] = nlsf_q15[0].max(delta_min_q15[0]);
    for i in 1..order {
        let min_allowed = nlsf_q15[i - 1].saturating_add(delta_min_q15[i]);
        nlsf_q15[i] = nlsf_q15[i].max(min_allowed);
    }
    let max_last = ((1 << 15) - delta_min_q15[order] as i32) as i16;
    nlsf_q15[order - 1] = nlsf_q15[order - 1].min(max_last);
    for i in (0..order - 1).rev() {
        let max_allowed = nlsf_q15[i + 1] - delta_min_q15[i + 1];
        nlsf_q15[i] = nlsf_q15[i].min(max_allowed);
    }
}

/// Escribe en `pred_q8[..order]` los coeficientes del predictor que eligen los selectores de
/// `ec_sel` del vector `cb1_index` del libro `codebook`.
fn unpack_predictors(pred_q8: &mut [u8; MAX_LPC_ORDER], codebook: &NlsfCodebook, cb1_index: usize) {
    let order = codebook.order;
    let base = cb1_index * order / 2;
    let entries = &codebook.ec_sel[base..base + order / 2];
    for i in (0..order).step_by(2) {
        let entry = entries[i / 2];
        for (j, &shift) in NLSF_EC_SEL_PRED_SHIFTS.iter().enumerate() {
            let predictor = ((entry >> shift) & 1) as usize;
            pred_q8[i + j] = codebook.pred_q8[i + predictor * (order - 1) + j];
        }
    }
}

/// Reconstruye en `res_q10[..order]` los residuos de la segunda etapa, en Q10, a partir de los
/// índices con signo `indices`, el paso `quant_step_size_q16` y el predictor `pred_q8`. Se
/// recorren del último al primero, porque cada uno se predice a partir del siguiente.
fn residual_dequant(
    res_q10: &mut [i16; MAX_LPC_ORDER],
    indices: &[i8],
    pred_q8: &[u8; MAX_LPC_ORDER],
    quant_step_size_q16: i32,
    order: usize,
) {
    let mut out_q10 = 0i32;
    for i in (0..order).rev() {
        let pred_q10 = (out_q10 * pred_q8[i] as i32) >> 8;
        out_q10 = (indices[i] as i32) << 10;
        if out_q10 > 0 {
            out_q10 -= NLSF_QUANT_LEVEL_ADJ_Q10;
        } else if out_q10 < 0 {
            out_q10 += NLSF_QUANT_LEVEL_ADJ_Q10;
        }
        out_q10 = pred_q10 + (((out_q10 as i64) * quant_step_size_q16 as i64) >> 16) as i32;
        res_q10[i] = out_q10 as i16;
    }
}

/// Limita `value` al intervalo cerrado `[min_value, max_value]`.
fn limit32(value: i32, min_value: i32, max_value: i32) -> i32 {
    value.clamp(min_value, max_value)
}

//! Funciones LPC de coma fija de SILK.

use super::fixed::{inverse32_var_q, rshift_round, rshift_round64, sat16, smmul, smulbb};
use super::tables::{LSF_COS_TAB, MAX_LPC_ORDER, ONE_Q16};

const QA: usize = 16;
const INV_PRED_GAIN_QA: usize = 24;
const MAX_LPC_STABILIZE_ITERATIONS: usize = 16;
const A_LIMIT_QA24: i32 = 16_773_022;
const MAX_PREDICTION_POWER_GAIN_INV_Q30: i32 = 107_374;
/// Respuesta en continua máxima de un filtro estable: 1,0 en Q12 (`LPC_inverse_pred_gain`).
const MAX_DC_RESPONSE_Q12: i32 = 4096;
/// Tope del coeficiente máximo en `silk_LPC_fit`: `(silk_int32_MAX >> 14) + silk_int16_MAX`.
const LPC_FIT_MAX_ABS: i32 = 163_838;
/// Chirp de partida de `silk_LPC_fit`: `SILK_FIX_CONST(0.999, 16)`.
const LPC_FIT_CHIRP_START_Q16: i32 = 65_470;

/// Convierte las NLSF estabilizadas `nlsf_q15`, en Q15, en coeficientes LPC en Q12, que escribe
/// en `lpc_q12[..order]` (`silk_NLSF2A`). `ordering` es el `lpc_ordering` del libro de códigos y
/// su longitud es el orden LPC. Si el filtro resultante es inestable, se le aplica expansión de
/// ancho de banda hasta `MAX_LPC_STABILIZE_ITERATIONS` veces.
pub(super) fn nlsf2a(lpc_q12: &mut [i16], nlsf_q15: &[i16], ordering: &[usize]) {
    let order = ordering.len();

    let mut cos_lsf_qa = [0i32; MAX_LPC_ORDER];
    for k in 0..order {
        let f_int = (nlsf_q15[k] as i32 >> 8) as usize;
        let f_frac = nlsf_q15[k] as i32 - ((f_int as i32) << 8);
        let cos_val = LSF_COS_TAB[f_int] as i32;
        let delta = LSF_COS_TAB[f_int + 1] as i32 - cos_val;
        let interp = (cos_val << 8) + delta * f_frac;
        cos_lsf_qa[ordering[k]] = rshift_round(interp, 20 - QA);
    }

    let dd = order / 2;
    let mut p = [0i32; MAX_LPC_ORDER / 2 + 1];
    let mut q = [0i32; MAX_LPC_ORDER / 2 + 1];
    nlsf2a_find_poly(&mut p, &cos_lsf_qa[0..], dd);
    nlsf2a_find_poly(&mut q, &cos_lsf_qa[1..], dd);

    let mut a32_qa1 = [0i32; MAX_LPC_ORDER];
    for k in 0..dd {
        let ptmp = p[k + 1] + p[k];
        let qtmp = q[k + 1] - q[k];
        a32_qa1[k] = -qtmp - ptmp;
        a32_qa1[order - k - 1] = qtmp - ptmp;
    }

    lpc_fit(lpc_q12, &mut a32_qa1, 12, QA + 1, order);
    for i in 0..MAX_LPC_STABILIZE_ITERATIONS {
        let inv_gain = lpc_inv_pred_gain(&lpc_q12[..order], order);
        if inv_gain != 0 {
            break;
        }
        bw_expand_32(&mut a32_qa1[..order], order, ONE_Q16 - (2 << i));
        for k in 0..order {
            lpc_q12[k] = rshift_round(a32_qa1[k], QA + 1 - 12) as i16;
        }
    }
}

/// Ganancia de predicción inversa, en Q30, de los coeficientes LPC `lpc_q12` de orden `order`,
/// en Q12. Devuelve 0 si el filtro es inestable o su respuesta en continua llega al máximo.
pub(super) fn lpc_inv_pred_gain(lpc_q12: &[i16], order: usize) -> i32 {
    let mut a_tmp_qa = [0i32; MAX_LPC_ORDER];
    let mut dc_resp = 0i32;
    for k in 0..order {
        dc_resp += lpc_q12[k] as i32;
        a_tmp_qa[k] = (lpc_q12[k] as i32) << (INV_PRED_GAIN_QA - 12);
    }
    if dc_resp >= MAX_DC_RESPONSE_Q12 {
        return 0;
    }
    lpc_inverse_pred_gain_qa(&mut a_tmp_qa[..order], order)
}

/// Expansión de ancho de banda de los coeficientes `ar` de orden `order`, en Q12: el coeficiente
/// `k` se multiplica por `chirp_q16` elevado a `k + 1`, con redondeo (`silk_bwexpander`). libopus
/// evita aquí `silk_SMULWB` porque su sesgo puede volver inestable el filtro.
pub(super) fn bwexpander(ar: &mut [i16], order: usize, chirp_q16: i32) {
    let mut chirp_q16 = chirp_q16;
    let chirp_minus_one_q16 = chirp_q16 - ONE_Q16;
    for coeff in ar.iter_mut().take(order.saturating_sub(1)) {
        *coeff = rshift_round(chirp_q16 * i32::from(*coeff), 16) as i16;
        chirp_q16 += rshift_round(chirp_q16.wrapping_mul(chirp_minus_one_q16), 16);
    }
    if let Some(last) = order.checked_sub(1) {
        ar[last] = rshift_round(chirp_q16 * i32::from(ar[last]), 16) as i16;
    }
}

/// Filtro de análisis LPC (`silk_LPC_analysis_filter`) con el que se reblanquean las tramas
/// sonoras y la historia de la ocultación: escribe en `output[..len]` el residuo en Q0 de `input` con los coeficientes `a_q12`
/// de orden `order`. Las `order` primeras muestras quedan a cero.
pub(super) fn lpc_analysis_filter(
    output: &mut [i16],
    input: &[i16],
    a_q12: &[i16],
    len: usize,
    order: usize,
) {
    debug_assert!(order <= len);
    output[..order].fill(0);
    for ix in order..len {
        let mut out32_q12 = smulbb(input[ix - 1] as i32, a_q12[0] as i32);
        out32_q12 = out32_q12.wrapping_add(smulbb(input[ix - 2] as i32, a_q12[1] as i32));
        out32_q12 = out32_q12.wrapping_add(smulbb(input[ix - 3] as i32, a_q12[2] as i32));
        out32_q12 = out32_q12.wrapping_add(smulbb(input[ix - 4] as i32, a_q12[3] as i32));
        out32_q12 = out32_q12.wrapping_add(smulbb(input[ix - 5] as i32, a_q12[4] as i32));
        out32_q12 = out32_q12.wrapping_add(smulbb(input[ix - 6] as i32, a_q12[5] as i32));
        for j in (6..order).step_by(2) {
            out32_q12 = out32_q12.wrapping_add(smulbb(input[ix - 1 - j] as i32, a_q12[j] as i32));
            out32_q12 =
                out32_q12.wrapping_add(smulbb(input[ix - 2 - j] as i32, a_q12[j + 1] as i32));
        }
        let out32_q12 = ((input[ix] as i32) << 12).wrapping_sub(out32_q12);
        output[ix] = sat16(rshift_round(out32_q12, 12));
    }
}

/// Construye en `out[..=dd]` el polinomio par o impar de `nlsf2a` a partir de los cosenos
/// entrelazados `c_lsf_qa`, tomados de dos en dos; `dd` es la mitad del orden.
fn nlsf2a_find_poly(out: &mut [i32; MAX_LPC_ORDER / 2 + 1], c_lsf_qa: &[i32], dd: usize) {
    out[0] = 1 << QA;
    out[1] = -c_lsf_qa[0];
    for k in 1..dd {
        let ftmp = c_lsf_qa[2 * k];
        out[k + 1] = (out[k - 1] << 1) - rshift_round64((ftmp as i64) * out[k] as i64, QA);
        for n in (2..=k).rev() {
            out[n] += out[n - 2] - rshift_round64((ftmp as i64) * out[n - 1] as i64, QA);
        }
        out[1] -= ftmp;
    }
}

/// Ajusta los coeficientes LPC intermedios `a_qin`, en Q`qin`, al rango de `i16` en Q`qout` y los
/// escribe en `lpc_q12[..order]` (`silk_LPC_fit`). Prueba hasta 10 expansiones de ancho de banda
/// sobre `a_qin`; si no bastan, recorta los coeficientes y deja también recortado `a_qin`.
fn lpc_fit(
    lpc_q12: &mut [i16],
    a_qin: &mut [i32; MAX_LPC_ORDER],
    qout: usize,
    qin: usize,
    order: usize,
) {
    let mut peak_index = 0usize;
    for _ in 0..10 {
        let mut max_abs = 0i32;
        for (idx, &value) in a_qin.iter().take(order).enumerate() {
            let abs_value = value.saturating_abs();
            if abs_value > max_abs {
                max_abs = abs_value;
                peak_index = idx;
            }
        }
        let shifted = rshift_round(max_abs, qin - qout);
        if shifted <= i16::MAX as i32 {
            for k in 0..order {
                lpc_q12[k] = rshift_round(a_qin[k], qin - qout) as i16;
            }
            return;
        }

        let capped = shifted.min(LPC_FIT_MAX_ABS);
        let numerator = ((capped - i16::MAX as i32) as i64) << 14;
        let denominator = ((capped as i64) * (peak_index as i64 + 1)) >> 2;
        let chirp_q16 = LPC_FIT_CHIRP_START_Q16 - (numerator / denominator) as i32;
        bw_expand_32(&mut a_qin[..order], order, chirp_q16);
    }

    for k in 0..order {
        let rounded = rshift_round(a_qin[k], qin - qout);
        let clipped = rounded.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        lpc_q12[k] = clipped;
        a_qin[k] = (clipped as i32) << (qin - qout);
    }
}

/// Expansión de ancho de banda sobre los coeficientes intermedios de 32 bits `ar_qa1` de orden
/// `order`: el coeficiente `k` se multiplica por `chirp_q16` elevado a `k + 1`.
fn bw_expand_32(ar_qa1: &mut [i32], order: usize, chirp_q16: i32) {
    let mut chirp_q16_local = chirp_q16;
    let chirp_minus_one_q16 = chirp_q16_local - ONE_Q16;
    for coeff in ar_qa1.iter_mut().take(order.saturating_sub(1)) {
        *coeff = ((*coeff as i64 * chirp_q16_local as i64) >> 16) as i32;
        chirp_q16_local += rshift_round(
            ((chirp_q16_local as i64) * (chirp_minus_one_q16 as i64)) as i32,
            16,
        );
    }
    ar_qa1[order - 1] = ((ar_qa1[order - 1] as i64 * chirp_q16_local as i64) >> 16) as i32;
}

/// Ganancia de predicción inversa de los coeficientes LPC `a_qa`, en Q24, de orden `order`
/// (`LPC_inverse_pred_gain_QA_c`). Usa `a_qa` como memoria de trabajo. Devuelve la ganancia en
/// Q30, o 0 si el filtro es inestable.
fn lpc_inverse_pred_gain_qa(a_qa: &mut [i32], order: usize) -> i32 {
    let mut inv_gain_q30 = 1 << 30;
    for k in (1..order).rev() {
        let coeff = a_qa[k];
        if !(-A_LIMIT_QA24..=A_LIMIT_QA24).contains(&coeff) {
            return 0;
        }

        let rc_q31 = -(coeff << (31 - INV_PRED_GAIN_QA));
        let rc_mult1_q30 = (1 << 30) - smmul(rc_q31, rc_q31);
        inv_gain_q30 = smmul(inv_gain_q30, rc_mult1_q30) << 2;
        if inv_gain_q30 < MAX_PREDICTION_POWER_GAIN_INV_Q30 {
            return 0;
        }

        let mult2_q = 32 - (rc_mult1_q30.unsigned_abs().leading_zeros() as usize);
        let rc_mult2 = inverse32_var_q(rc_mult1_q30, mult2_q + 30);
        for n in 0..((k + 1) >> 1) {
            let tmp1 = a_qa[n];
            let tmp2 = a_qa[k - n - 1];
            let lhs = tmp1.saturating_sub(mul32_frac_q(tmp2, rc_q31, 31));
            let rhs = tmp2.saturating_sub(mul32_frac_q(tmp1, rc_q31, 31));
            let next0 = rshift_round64((lhs as i64) * rc_mult2 as i64, mult2_q);
            let next1 = rshift_round64((rhs as i64) * rc_mult2 as i64, mult2_q);
            a_qa[n] = next0;
            a_qa[k - n - 1] = next1;
        }
    }

    if !(-A_LIMIT_QA24..=A_LIMIT_QA24).contains(&a_qa[0]) {
        return 0;
    }
    let rc_q31 = -(a_qa[0] << (31 - INV_PRED_GAIN_QA));
    let rc_mult1_q30 = (1 << 30) - smmul(rc_q31, rc_q31);
    inv_gain_q30 = smmul(inv_gain_q30, rc_mult1_q30) << 2;
    if inv_gain_q30 < MAX_PREDICTION_POWER_GAIN_INV_Q30 {
        0
    } else {
        inv_gain_q30
    }
}

/// Producto de `a` por la fracción `b` en Q`q`, redondeado: `(a * b) >> q`.
fn mul32_frac_q(a: i32, b: i32, q: usize) -> i32 {
    rshift_round64((a as i64) * (b as i64), q)
}

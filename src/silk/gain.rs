//! Decuantificación de las ganancias de SILK.

use super::fixed::{smlawb, smulbb, smulwb};
use super::tables::{MAX_DELTA_GAIN_QUANT, MIN_DELTA_GAIN_QUANT, N_LEVELS_QGAIN};

const MIN_QGAIN_DB: i32 = 2;
const MAX_QGAIN_DB: i32 = 88;
const OFFSET: i32 = ((MIN_QGAIN_DB * 128) / 6) + 16 * 128;
const INV_SCALE_Q16: i32 =
    (super::tables::ONE_Q16 * (((MAX_QGAIN_DB - MIN_QGAIN_DB) * 128) / 6)) / (N_LEVELS_QGAIN - 1);
/// La ganancia absoluta de la primera subtrama no puede bajar más de 16 pasos (unos 21,8 dB)
/// respecto a la anterior (`silk_gains_dequant`).
const MAX_GAIN_INDEX_DROP: i32 = 16;
/// Entrada de `silk_log2lin` a partir de la cual el resultado satura: 31 en Q7, menos 1.
const LOG2LIN_SATURATION_Q7: i32 = 3967;
/// Por debajo de 16 en Q7 la salida cabe en 16 bits y se usa la rama exacta de `silk_log2lin`.
const LOG2LIN_SMALL_OUTPUT_Q7: i32 = 2048;
/// 1,0 en Q7 y coeficiente de la aproximación polinómica de la parte fraccionaria
/// (`silk_log2lin`).
const LOG2LIN_ONE_Q7: i32 = 128;
const LOG2LIN_CURVE_COEF: i32 = -174;

/// Convierte los índices de ganancia `gain_indices` de las `nb_subfr` subtramas en ganancias
/// lineales en Q16 (`silk_gains_dequant`). Sin `conditional`, el índice de la primera subtrama es
/// absoluto; los demás son incrementos sobre el anterior, que parte de `prev_index`. Guarda en
/// `prev_index` el último índice.
pub(super) fn decode_gains(
    gain_indices: &[i8],
    prev_index: &mut i8,
    conditional: bool,
    nb_subfr: usize,
) -> [i32; super::tables::MAX_NB_SUBFR] {
    let mut gains_q16 = [0i32; super::tables::MAX_NB_SUBFR];
    let mut prev = *prev_index as i32;

    for k in 0..nb_subfr {
        if k == 0 && !conditional {
            prev = (gain_indices[k] as i32).max(prev - MAX_GAIN_INDEX_DROP);
        } else {
            let ind_tmp = gain_indices[k] as i32 + MIN_DELTA_GAIN_QUANT;
            let double_step_size_threshold = 2 * MAX_DELTA_GAIN_QUANT - N_LEVELS_QGAIN + prev;
            if ind_tmp > double_step_size_threshold {
                prev += (ind_tmp << 1) - double_step_size_threshold;
            } else {
                prev += ind_tmp;
            }
        }

        prev = prev.clamp(0, N_LEVELS_QGAIN - 1);
        gains_q16[k] = log2lin(smulwb(INV_SCALE_Q16, prev) + OFFSET);
    }

    *prev_index = prev as i8;
    gains_q16
}

/// Pasa del dominio logarítmico al lineal (`silk_log2lin`): aproxima `2^(in_log_q7 / 128)`, con
/// `in_log_q7` en Q7. Devuelve 0 si la entrada es negativa e `i32::MAX` si satura.
fn log2lin(in_log_q7: i32) -> i32 {
    if in_log_q7 < 0 {
        return 0;
    }
    if in_log_q7 >= LOG2LIN_SATURATION_Q7 {
        return i32::MAX;
    }

    let mut out = 1i32 << (in_log_q7 >> 7);
    let frac_q7 = in_log_q7 & 0x7F;
    let curve = smlawb(
        frac_q7,
        smulbb(frac_q7, LOG2LIN_ONE_Q7 - frac_q7),
        LOG2LIN_CURVE_COEF,
    );
    if in_log_q7 < LOG2LIN_SMALL_OUTPUT_Q7 {
        // `silk_ADD_RSHIFT32`: desplazamiento sin redondeo. Aquí `out` < 2^16 y `curve` < 128,
        // así que las operaciones saturadas nunca saturan y coinciden con las de libopus.
        out = out.saturating_add(out.saturating_mul(curve) >> 7);
    } else {
        out = out.saturating_add((out >> 7).saturating_mul(curve));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::log2lin;

    /// `silk_log2lin` de libopus (`silk/log2lin.c`) en `i64`, como oráculo: en la rama de
    /// entradas menores que 2048 suma `(out * curva) >> 7` sin redondeo (`silk_ADD_RSHIFT32`).
    fn libopus_log2lin(in_log_q7: i32) -> i32 {
        if in_log_q7 < 0 {
            return 0;
        }
        if in_log_q7 >= 3967 {
            return i32::MAX;
        }
        let out = 1i64 << (in_log_q7 >> 7);
        let frac_q7 = i64::from(in_log_q7 & 0x7F);
        let curve = frac_q7 + ((frac_q7 * (128 - frac_q7) * -174) >> 16);
        let out = if in_log_q7 < 2048 {
            out + ((out * curve) >> 7)
        } else {
            out + (out >> 7) * curve
        };
        out as i32
    }

    #[test]
    fn log2lin_matches_libopus() {
        for in_log_q7 in -1..=4000 {
            assert_eq!(
                log2lin(in_log_q7),
                libopus_log2lin(in_log_q7),
                "entrada {in_log_q7}"
            );
        }
    }
}

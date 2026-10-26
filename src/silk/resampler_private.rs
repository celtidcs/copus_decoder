//! Piezas internas del remuestreador de SILK.

use super::fixed::{rshift_round, sat16, smlabb, smlawb, smulbb, smulwb};
use super::resampler::SILK_RESAMPLER_MAX_IIR_ORDER;
use super::resampler_rom::{
    RESAMPLER_FIR_FRACTIONS, RESAMPLER_FRAC_FIR_12, RESAMPLER_ORDER_FIR_12, RESAMPLER_UP2_HQ_0,
    RESAMPLER_UP2_HQ_1,
};

/// Duplica la frecuencia de las `len` primeras muestras de `input` con los dos filtros paso
/// todo de alta calidad de SILK, cuyo estado en Q10 es `state_q10`, y escribe las `2 * len`
/// muestras en `output`.
pub(super) fn up2_hq(
    state_q10: &mut [i32; SILK_RESAMPLER_MAX_IIR_ORDER],
    output: &mut [i16],
    input: &[i16],
    len: usize,
) {
    for k in 0..len {
        let in32 = (input[k] as i32) << 10;

        let mut y = in32 - state_q10[0];
        let mut x = smulwb(y, RESAMPLER_UP2_HQ_0[0] as i32);
        let mut out32_1 = state_q10[0] + x;
        state_q10[0] = in32 + x;

        y = out32_1 - state_q10[1];
        x = smulwb(y, RESAMPLER_UP2_HQ_0[1] as i32);
        let out32_2 = state_q10[1] + x;
        state_q10[1] = out32_1 + x;

        y = out32_2 - state_q10[2];
        x = smlawb(y, y, RESAMPLER_UP2_HQ_0[2] as i32);
        out32_1 = state_q10[2] + x;
        state_q10[2] = out32_2 + x;
        output[2 * k] = sat16(rshift_round(out32_1, 10));

        y = in32 - state_q10[3];
        x = smulwb(y, RESAMPLER_UP2_HQ_1[0] as i32);
        out32_1 = state_q10[3] + x;
        state_q10[3] = in32 + x;

        y = out32_1 - state_q10[4];
        x = smulwb(y, RESAMPLER_UP2_HQ_1[1] as i32);
        let out32_2 = state_q10[4] + x;
        state_q10[4] = out32_1 + x;

        y = out32_2 - state_q10[5];
        x = smlawb(y, y, RESAMPLER_UP2_HQ_1[2] as i32);
        out32_1 = state_q10[5] + x;
        state_q10[5] = out32_2 + x;
        output[2 * k + 1] = sat16(rshift_round(out32_1, 10));
    }
}

/// Interpola con el FIR fraccionario la señal duplicada de `buffer`, precedida de su historia,
/// a la frecuencia de salida: avanza `index_increment_q16` por muestra de salida mientras la
/// posición en Q16 sea menor que `max_index_q16`. Devuelve las muestras escritas en `output`.
pub(super) fn iir_fir_interpol(
    output: &mut [i16],
    buffer: &[i16],
    max_index_q16: i32,
    index_increment_q16: i32,
) -> usize {
    let mut out_idx = 0usize;
    let mut index_q16 = 0i32;
    while index_q16 < max_index_q16 {
        let table_index = smulwb(index_q16 & 0xFFFF, RESAMPLER_FIR_FRACTIONS as i32) as usize;
        // Los 4 últimos taps son los 4 primeros de la fase simétrica, recorridos al revés.
        let mirror_index = RESAMPLER_FIR_FRACTIONS - 1 - table_index;
        let buf_index = (index_q16 >> 16) as usize;
        let buf_ptr = &buffer[buf_index..];

        let mut res_q15 = smulbb(
            buf_ptr[0] as i32,
            RESAMPLER_FRAC_FIR_12[table_index][0] as i32,
        );
        res_q15 = smlabb(
            res_q15,
            buf_ptr[1] as i32,
            RESAMPLER_FRAC_FIR_12[table_index][1] as i32,
        );
        res_q15 = smlabb(
            res_q15,
            buf_ptr[2] as i32,
            RESAMPLER_FRAC_FIR_12[table_index][2] as i32,
        );
        res_q15 = smlabb(
            res_q15,
            buf_ptr[3] as i32,
            RESAMPLER_FRAC_FIR_12[table_index][3] as i32,
        );
        res_q15 = smlabb(
            res_q15,
            buf_ptr[4] as i32,
            RESAMPLER_FRAC_FIR_12[mirror_index][3] as i32,
        );
        res_q15 = smlabb(
            res_q15,
            buf_ptr[5] as i32,
            RESAMPLER_FRAC_FIR_12[mirror_index][2] as i32,
        );
        res_q15 = smlabb(
            res_q15,
            buf_ptr[6] as i32,
            RESAMPLER_FRAC_FIR_12[mirror_index][1] as i32,
        );
        res_q15 = smlabb(
            res_q15,
            buf_ptr[7] as i32,
            RESAMPLER_FRAC_FIR_12[mirror_index][0] as i32,
        );
        output[out_idx] = sat16(rshift_round(res_q15, 15));
        out_idx += 1;
        index_q16 += index_increment_q16;
    }
    out_idx
}

/// Longitud en muestras de la historia del interpolador FIR.
pub(super) const fn fir_history_len() -> usize {
    RESAMPLER_ORDER_FIR_12
}

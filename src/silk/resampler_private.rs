//! Piezas internas del remuestreador de SILK.

use super::fixed::{rshift_round, sat16, smlabb, smlawb, smulbb, smulwb};
use super::resampler::SILK_RESAMPLER_MAX_IIR_ORDER;
use super::resampler_rom::{
    RESAMPLER_DOWN_ORDER_FIR0, RESAMPLER_DOWN_ORDER_FIR1, RESAMPLER_FIR_FRACTIONS,
    RESAMPLER_FRAC_FIR_12, RESAMPLER_ORDER_FIR_12, RESAMPLER_UP2_HQ_0, RESAMPLER_UP2_HQ_1,
};

/// Desplazamiento de la salida del filtro AR y del FIR de bajada: trabajan en Q8 y el FIR deja
/// el resultado en Q6 (`silk_RSHIFT_ROUND( res_Q6, 6 )`).
const DOWN_FIR_OUTPUT_SHIFT: usize = 6;
/// Desplazamiento de la entrada del filtro AR a Q8 (`silk_ADD_LSHIFT32( S[ 0 ], in[ k ], 8 )`).
const AR2_INPUT_SHIFT: u32 = 8;
/// Desplazamiento de la salida del filtro AR antes de aplicarle los coeficientes en Q14.
const AR2_FEEDBACK_SHIFT: u32 = 2;

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

/// Forma del FIR de una bajada del decodificador, como `FIR_Order` y `FIR_Fracs` de
/// `silk_resampler_init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DownFirShape {
    /// Orden 18 con `fracs` fases (3/4 y 2/3, `RESAMPLER_DOWN_ORDER_FIR0`).
    Phased { fracs: usize },
    /// Orden 24 con una sola fase simétrica (1/2, `RESAMPLER_DOWN_ORDER_FIR1`).
    Symmetric,
}

impl DownFirShape {
    /// Orden del FIR: las muestras filtradas que se guardan entre bloques.
    pub(super) const fn order(self) -> usize {
        match self {
            Self::Phased { .. } => RESAMPLER_DOWN_ORDER_FIR0,
            Self::Symmetric => RESAMPLER_DOWN_ORDER_FIR1,
        }
    }
}

/// Filtro AR de segundo orden con elementos de un solo retardo (`silk_resampler_private_AR2`):
/// filtra las `input.len()` muestras de `input` con los coeficientes en Q14 `a_q14` y su
/// estado `state`, y escribe la salida en Q8 en `out_q8`.
pub(super) fn ar2(state: &mut [i32], out_q8: &mut [i32], input: &[i16], a_q14: &[i16]) {
    for (out, &sample) in out_q8.iter_mut().zip(input) {
        let out32 = state[0].wrapping_add(i32::from(sample) << AR2_INPUT_SHIFT);
        *out = out32;
        let out32 = out32.wrapping_shl(AR2_FEEDBACK_SHIFT);
        state[0] = smlawb(state[1], out32, i32::from(a_q14[0]));
        state[1] = smulwb(out32, i32::from(a_q14[1]));
    }
}

/// Interpola con el FIR de bajada `fir_coefs` (sin los dos coeficientes del AR) la señal filtrada
/// de `buffer`, precedida de su historia: avanza `index_increment_q16` por muestra de salida
/// mientras la posición en Q16 sea menor que `max_index_q16`
/// (`silk_resampler_private_down_FIR_INTERPOL`). Devuelve las muestras escritas en `output`.
pub(super) fn down_fir_interpol(
    output: &mut [i16],
    buffer: &[i32],
    fir_coefs: &[i16],
    shape: DownFirShape,
    max_index_q16: i32,
    index_increment_q16: i32,
) -> usize {
    let mut out_idx = 0usize;
    let mut index_q16 = 0i32;
    while index_q16 < max_index_q16 {
        let buf = &buffer[(index_q16 >> 16) as usize..];
        let res_q6 = match shape {
            DownFirShape::Phased { fracs } => {
                let phase = smulwb(index_q16 & 0xFFFF, fracs as i32) as usize;
                phased_fir(buf, fir_coefs, phase, fracs)
            }
            DownFirShape::Symmetric => symmetric_fir(buf, fir_coefs),
        };
        output[out_idx] = sat16(rshift_round(res_q6, DOWN_FIR_OUTPUT_SHIFT));
        out_idx += 1;
        index_q16 += index_increment_q16;
    }
    out_idx
}

/// Producto del FIR de orden 18 de la fase `phase` (de `fracs`): la primera mitad de los
/// coeficientes es la de esa fase y la segunda, la de la fase simétrica recorrida al revés.
fn phased_fir(buf: &[i32], fir_coefs: &[i16], phase: usize, fracs: usize) -> i32 {
    let half = RESAMPLER_DOWN_ORDER_FIR0 / 2;
    let taps = &fir_coefs[half * phase..half * (phase + 1)];
    let mirror = &fir_coefs[half * (fracs - 1 - phase)..half * (fracs - phase)];
    let mut res_q6 = smulwb(buf[0], i32::from(taps[0]));
    for i in 1..half {
        res_q6 = smlawb(res_q6, buf[i], i32::from(taps[i]));
    }
    for (i, &coef) in mirror.iter().enumerate() {
        res_q6 = smlawb(
            res_q6,
            buf[RESAMPLER_DOWN_ORDER_FIR0 - 1 - i],
            i32::from(coef),
        );
    }
    res_q6
}

/// Producto del FIR simétrico de orden 24: cada coeficiente multiplica la suma de las dos
/// muestras que equidistan del centro.
fn symmetric_fir(buf: &[i32], fir_coefs: &[i16]) -> i32 {
    let last = RESAMPLER_DOWN_ORDER_FIR1 - 1;
    let mut res_q6 = smulwb(buf[0].wrapping_add(buf[last]), i32::from(fir_coefs[0]));
    for i in 1..RESAMPLER_DOWN_ORDER_FIR1 / 2 {
        res_q6 = smlawb(
            res_q6,
            buf[i].wrapping_add(buf[last - i]),
            i32::from(fir_coefs[i]),
        );
    }
    res_q6
}

/// Longitud en muestras de la historia del interpolador FIR.
pub(super) const fn fir_history_len() -> usize {
    RESAMPLER_ORDER_FIR_12
}

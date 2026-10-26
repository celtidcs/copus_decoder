//! Ocultación de pérdidas de SILK.
//!
//! Guarda de cada trama interna decodificada lo que necesita la ocultación y sintetiza las
//! tramas internas ocultadas de una en una.

use super::fixed::{add_sat32, rshift_round, sat16, silk_rand, smlawb, smulww};
use super::tables::{MAX_FRAME_LENGTH, MAX_LPC_ORDER, MAX_NB_SUBFR};

/// Ganancia de ruido inicial: 1,0 en Q14.
const SHAPE_GAIN_INITIAL_Q14: i32 = 1 << 14;
/// Atenuación por pérdida consecutiva: la trama `n` sale con `(10 - min(n, 9)) / 10`.
/// Valor heredado de la 0.1.1; esta ocultación no es la de libopus. **No comprobado**.
const PLC_ATTENUATION_STEPS: i32 = 10;
/// Escala del ruido por kHz de la frecuencia interna, en Q10. Heredado; **no comprobado**.
const PLC_NOISE_SCALE_PER_KHZ: i32 = 3;
const PLC_NOISE_SCALE_SHIFT: i32 = 10;
/// Decaimiento de la ganancia de ruido por trama: 0,99 en Q14, el mismo factor que
/// `PLC_RAND_ATTENUATE_UV_Q15` de libopus. Heredado; **no comprobado**.
const SHAPE_GAIN_DECAY_Q14: i64 = 16_220;

/// Datos de una trama interna de SILK decodificada con los que se actualiza la ocultación.
#[derive(Debug, Clone)]
pub(super) struct SilkDecodedFrame {
    /// Lag de tono de la última subtrama de una trama sonora, o cero si no es sonora.
    pub lag: i32,
    /// Ganancia de tono de cada subtrama, reducida a un valor, en Q16.
    pub ltp_gains: [i32; MAX_NB_SUBFR],
    /// PCM sintetizado de la trama, a la frecuencia interna.
    pub output: Vec<i16>,
}

/// Estado de la ocultación de pérdidas de un canal de SILK. Lo hereda de la 0.1.1 y no es el de
/// libopus.
#[derive(Debug, Clone)]
pub(super) struct SilkPlcState {
    /// Lag de tono de la última trama decodificada, en muestras a la frecuencia interna; cero si
    /// no era sonora.
    pub pitch_lag: i32,
    /// Ganancia de tono de cada subtrama, en Q16.
    pub pitch_gain: [i32; MAX_NB_SUBFR],
    /// Coeficientes LPC guardados, en Q12.
    pub lpc_coeffs: [i32; MAX_LPC_ORDER],
    /// Ganancia del ruido de la excitación, en Q14; decae tras cada trama ocultada.
    pub shape_gain: i32,
    /// Semilla del generador pseudoaleatorio de la ocultación.
    pub rand_seed: i32,
    /// Última trama sintetizada a la frecuencia interna, de la que se repite el tono.
    pub last_frame_out: Vec<i16>,
    /// Subtramas de una trama interna ocultada.
    pub nb_subfr: usize,
    /// Muestras por subtrama a la frecuencia interna.
    pub subfr_length: usize,
    /// Frecuencia interna, en kHz.
    pub fs_khz: i32,
    /// Orden LPC.
    pub nb_coefs: usize,
}

impl Default for SilkPlcState {
    /// Estado sin trama previa: todo a cero salvo la ganancia de ruido inicial y la semilla, 1.
    fn default() -> Self {
        Self {
            pitch_lag: 0,
            pitch_gain: [0; MAX_NB_SUBFR],
            lpc_coeffs: [0; MAX_LPC_ORDER],
            shape_gain: SHAPE_GAIN_INITIAL_Q14,
            rand_seed: 1,
            last_frame_out: Vec::new(),
            nb_subfr: 0,
            subfr_length: 0,
            fs_khz: 0,
            nb_coefs: 0,
        }
    }
}

/// Actualiza `plc` con la trama decodificada `frame`: lag de tono, ganancias acotadas a
/// `0..=1,0` en Q16, los `nb_coefs` coeficientes LPC de `lpc_q12`, la forma de la trama
/// (`nb_subfr` subtramas de `subfr_length` muestras), `fs_khz` y el PCM de la trama.
pub(super) fn plc_update(
    plc: &mut SilkPlcState,
    frame: &SilkDecodedFrame,
    lpc_q12: &[i32],
    nb_subfr: usize,
    subfr_length: usize,
    fs_khz: i32,
    nb_coefs: usize,
) {
    plc.pitch_lag = frame.lag.max(0);
    plc.pitch_gain.fill(0);
    for (dst, src) in plc
        .pitch_gain
        .iter_mut()
        .zip(frame.ltp_gains.iter())
        .take(nb_subfr.min(MAX_NB_SUBFR))
    {
        *dst = (*src).clamp(0, 1 << 16);
    }

    plc.lpc_coeffs.fill(0);
    for (dst, src) in plc
        .lpc_coeffs
        .iter_mut()
        .zip(lpc_q12.iter())
        .take(nb_coefs.min(MAX_LPC_ORDER))
    {
        *dst = *src;
    }

    plc.nb_subfr = nb_subfr;
    plc.subfr_length = subfr_length;
    plc.fs_khz = fs_khz;
    plc.nb_coefs = nb_coefs.min(MAX_LPC_ORDER);
    plc.last_frame_out.clear();
    plc.last_frame_out.extend_from_slice(&frame.output);
}

/// Sintetiza en `out` una trama interna ocultada: repetición del tono de la última trama más
/// ruido, por el filtro LPC que parte de la memoria `lpc_state`, atenuado según las pérdidas
/// consecutivas `loss_count`. Actualiza `lpc_state`, la última trama y la ganancia de ruido de
/// `plc`. Si el estado no describe una trama o `out` no mide una trama, `out` queda a cero.
pub(super) fn plc_conceal(
    plc: &mut SilkPlcState,
    lpc_state: &mut [i32],
    out: &mut [i16],
    loss_count: u32,
) {
    let frame_len = plc.subfr_length.saturating_mul(plc.nb_subfr);
    let nb_coefs = plc.nb_coefs.min(MAX_LPC_ORDER).min(lpc_state.len());
    if frame_len == 0 || out.len() != frame_len || nb_coefs == 0 {
        out.fill(0);
        return;
    }

    let attenuate_q16 = if loss_count == 0 {
        1 << 16
    } else {
        let remaining =
            PLC_ATTENUATION_STEPS - (loss_count.min(PLC_ATTENUATION_STEPS as u32 - 1)) as i32;
        ((1 << 16) * remaining) / PLC_ATTENUATION_STEPS
    };
    let base_rand_scale_q16 =
        (plc.fs_khz.max(1) * PLC_NOISE_SCALE_PER_KHZ) << PLC_NOISE_SCALE_SHIFT;
    let noise_gain_q16 = mul_q14_q16(plc.shape_gain, base_rand_scale_q16);
    let mut synth_q14 = [0i32; MAX_LPC_ORDER + MAX_FRAME_LENGTH];
    synth_q14[..nb_coefs].copy_from_slice(&lpc_state[..nb_coefs]);

    for subframe_idx in 0..plc.nb_subfr {
        let gain_q16 = plc.pitch_gain[subframe_idx.min(MAX_NB_SUBFR - 1)];
        let pitch_offset = subframe_idx * plc.subfr_length;
        for sample_idx in 0..plc.subfr_length {
            let frame_idx = pitch_offset + sample_idx;
            let pitch_sample = plc_pitch_sample(plc, frame_idx);
            let mut excitation = smulww(pitch_sample, gain_q16);

            plc.rand_seed = silk_rand(plc.rand_seed);
            let noise = (plc.rand_seed >> 16) as i16 as i32;
            excitation = excitation.wrapping_add(smulww(noise, noise_gain_q16));

            let hist_idx = nb_coefs + frame_idx;
            let mut lpc_pred_q10 = (nb_coefs as i32) >> 1;
            for coef_idx in 0..nb_coefs {
                lpc_pred_q10 = smlawb(
                    lpc_pred_q10,
                    synth_q14[hist_idx - 1 - coef_idx],
                    plc.lpc_coeffs[coef_idx],
                );
            }

            let current_q14 = smulww(
                add_sat32(
                    lshift_sat32_widened(excitation, 14),
                    lshift_sat32_widened(lpc_pred_q10, 4),
                ),
                attenuate_q16,
            );
            synth_q14[hist_idx] = current_q14;
            out[frame_idx] = sat16(rshift_round(current_q14, 14));
        }
    }

    lpc_state[..nb_coefs].copy_from_slice(&synth_q14[frame_len..frame_len + nb_coefs]);
    plc.last_frame_out.clear();
    plc.last_frame_out.extend_from_slice(out);
    plc.shape_gain = ((plc.shape_gain as i64 * SHAPE_GAIN_DECAY_Q14) >> 14) as i32;
}

/// Muestra de la última trama guardada que repite el tono en la posición `frame_idx` de la trama
/// ocultada: la de índice `frame_idx - pitch_lag`, circular en la longitud de la trama guardada.
/// Devuelve cero sin lag o sin trama guardada.
fn plc_pitch_sample(plc: &SilkPlcState, frame_idx: usize) -> i32 {
    if plc.pitch_lag <= 0 || plc.last_frame_out.is_empty() {
        return 0;
    }

    let history_len = plc.last_frame_out.len();
    let lag = plc.pitch_lag as usize % history_len;
    // Se suma la longitud antes de restar: una resta con vuelta en `usize` seguida del módulo
    // solo da el índice circular si la longitud divide a 2^64 (potencia de dos).
    let lag_idx = (frame_idx % history_len + history_len - lag) % history_len;
    plc.last_frame_out[lag_idx] as i32
}

/// Producto de `value_q14`, en Q14, por `value_q16`, en Q16; el resultado queda en Q16.
fn mul_q14_q16(value_q14: i32, value_q16: i32) -> i32 {
    ((value_q14 as i64 * value_q16 as i64) >> 14) as i32
}

/// Desplazamiento a la izquierda con saturación, calculado en 64 bits: satura a `i32::MAX` o
/// `i32::MIN` exactos. **No es** `silk_LSHIFT_SAT32` (`fixed::lshift_sat32`), que limita antes
/// de desplazar y satura a `(i32::MAX >> shift) << shift`. Heredado de la 0.1.1 en esta
/// ocultación, que no es la de libopus; unificarlo cambiaría la salida y va aparte.
fn lshift_sat32_widened(value: i32, shift: usize) -> i32 {
    let widened = (value as i64) << shift;
    widened.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::{SilkPlcState, plc_pitch_sample};

    /// Longitudes de la trama guardada de las pruebas: las de 10 y 20 ms a 8, 12 y 16 kHz, más
    /// una potencia de dos, el único caso en que la resta con vuelta ya era circular.
    const HISTORY_LENS: [usize; 4] = [160, 240, 256, 320];

    /// Índice circular de `frame_idx - lag` en un historial de `history_len`, con enteros con
    /// signo: el oráculo del contrato de `plc_pitch_sample`.
    fn circular_index(frame_idx: usize, lag: usize, history_len: usize) -> usize {
        (frame_idx as i64 - lag as i64).rem_euclid(history_len as i64) as usize
    }

    /// La muestra devuelta es la de `frame_idx - pitch_lag` en la trama guardada, dando la vuelta
    /// por el final, para cualquier longitud y para lags de hasta dos veces la trama.
    #[test]
    fn pitch_sample_wraps_circularly() {
        for history_len in HISTORY_LENS {
            let mut plc = SilkPlcState {
                last_frame_out: (0..history_len).map(|i| i as i16).collect(),
                ..SilkPlcState::default()
            };
            for lag in 1..=2 * history_len {
                plc.pitch_lag = lag as i32;
                for frame_idx in 0..history_len {
                    let expected = circular_index(frame_idx, lag, history_len) as i32;
                    assert_eq!(
                        plc_pitch_sample(&plc, frame_idx),
                        expected,
                        "historial {history_len}, lag {lag}, posición {frame_idx}"
                    );
                }
            }
        }
    }
}

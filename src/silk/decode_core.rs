//! Reconstrucción de la excitación de SILK y síntesis LPC.

use super::cng::CngState;
use super::fixed::{
    add_sat32, div32_var_q, inverse32_var_q, lshift_sat32, rshift_round, sat16, silk_rand, smlawb,
    smulwb, smulww,
};
use super::gain;
use super::lpc::lpc_analysis_filter;
use super::ltp;
use super::pitch;
use super::plc::SilkPlcState;
use super::tables::{
    LTP_MEM_LENGTH_MS, LTP_ORDER, MAX_FRAME_LENGTH, MAX_LPC_ORDER, MAX_NB_SUBFR,
    MAX_SUB_FRAME_LENGTH, ONE_Q16, QUANT_LEVEL_ADJUST_Q10, QUANTIZATION_OFFSETS_Q10,
    TYPE_NO_VOICE_ACTIVITY, TYPE_VOICED,
};

/// Valores iniciales del estado del canal: ganancia anterior 1,0 en Q16 (`silk_init_decoder`),
/// último índice de ganancia 10 y lag anterior 100 (`silk_decoder_set_fs`).
const INITIAL_LAST_GAIN_INDEX: i8 = 10;
const INITIAL_LAG_PREV: i32 = 100;
/// Coeficiente central del filtro de tono con que se suaviza el paso de una ocultación sonora a
/// una trama no sonora: 0,25 en Q14 (`silk_decode_core`).
const VOICED_TO_UNVOICED_LTP_Q14: i16 = 4_096;
/// Subtramas que dura ese suavizado (`MAX_NB_SUBFR / 2`).
const VOICED_TO_UNVOICED_SUBFRAMES: usize = MAX_NB_SUBFR / 2;

/// Control de una trama interna decodificada (`silk_decoder_control`): lo que la síntesis saca
/// de los parámetros y necesitan después la ocultación y el ruido de confort.
#[derive(Debug, Clone, Default)]
pub(super) struct FrameControl {
    /// Ganancia de cada subtrama, en Q16.
    pub gains_q16: [i32; MAX_NB_SUBFR],
    /// Lag de tono de cada subtrama; cero en las no sonoras.
    pub pitch_l: [i32; MAX_NB_SUBFR],
    /// Coeficientes del filtro de tono de cada subtrama, en Q14.
    pub ltp_coef_q14: [[i16; LTP_ORDER]; MAX_NB_SUBFR],
    /// Escala LTP de la trama, en Q14; cero si no es sonora.
    pub ltp_scale_q14: i32,
}

/// Parámetros decodificados de una trama interna de SILK que necesita la síntesis.
#[derive(Debug, Clone)]
pub(super) struct SilkFrameParams {
    /// Tipo de señal de la trama interna.
    pub signal_type: i32,
    /// Tipo de desplazamiento del cuantificador.
    pub quant_offset_type: i32,
    /// Índice de ganancia de cada subtrama.
    pub gain_indices: [i8; MAX_NB_SUBFR],
    /// Índice absoluto del lag de tono, en tramas sonoras.
    pub lag_index: i16,
    /// Índice de la tabla de contorno del tono, en tramas sonoras.
    pub contour_index: i8,
    /// Clase de periodicidad del LTP.
    pub ltp_periodicity: super::ltp::LtpPeriodicity,
    /// Índice del libro de códigos LTP de cada subtrama.
    pub ltp_indices: [i8; MAX_NB_SUBFR],
    /// Selector de la escala LTP.
    pub ltp_scale_index: i8,
    /// Semilla del generador pseudoaleatorio de la excitación.
    pub seed: i8,
    /// Pulsos de la excitación, con signo.
    pub pulses: Vec<i16>,
    /// Si las ganancias se decodifican de forma condicional.
    pub conditional: bool,
    /// Longitud de la trama interna, en muestras.
    pub frame_length: usize,
    /// Número de subtramas de la trama.
    pub nb_subfr: usize,
    /// Frecuencia interna, en kHz.
    pub fs_khz: u32,
    /// Orden LPC del ancho de banda actual.
    pub lpc_order: usize,
}

/// Estado persistente de la síntesis SILK de un canal codificado.
#[derive(Debug, Clone)]
pub(super) struct SilkChannelState {
    /// Ganancia lineal anterior, en Q16.
    pub prev_gain_q16: i32,
    /// Índice de ganancia anterior.
    pub last_gain_index: i8,
    /// Tipo de señal de la trama decodificada anterior.
    pub prev_signal_type: i32,
    /// Lag de tono anterior.
    pub lag_prev: i32,
    /// Memoria del filtro de síntesis LPC, en Q14.
    pub s_lpc_q14_buf: [i32; MAX_LPC_ORDER],
    /// Salida reciente del canal, con la que se reblanquea la historia del filtro de tono en las
    /// tramas sonoras.
    pub out_buf: [i16; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH],
    /// Excitación de la última trama decodificada, en Q14 (`exc_Q14`). Cada trama escribe solo
    /// sus muestras: detrás queda lo de las anteriores, del que la ocultación también saca ruido.
    pub exc_q14: [i32; MAX_FRAME_LENGTH],
    /// Estado de la ocultación de pérdidas, que se actualiza tras cada trama decodificada.
    pub plc: SilkPlcState,
    /// Estado del ruido de confort.
    pub cng: CngState,
}

impl SilkChannelState {
    /// Añade la trama ocultada `frame` a la salida reciente, como una decodificada
    /// (`silk_decode_frame`), a la frecuencia interna `fs_khz`.
    pub(super) fn record_concealed_frame(&mut self, frame: &[i16], fs_khz: u32) {
        let ltp_mem_length = fs_khz as usize * LTP_MEM_LENGTH_MS;
        push_output(&mut self.out_buf, ltp_mem_length, frame);
    }

    /// Tras una pérdida, la primera trama buena no limita cuánto baja la ganancia, para que la
    /// energía no rebote si se perdieron paquetes mientras bajaba (`silk_Decode` de libopus).
    pub(super) fn forget_last_gain_after_loss(&mut self) {
        self.last_gain_index = INITIAL_LAST_GAIN_INDEX;
    }

    /// Reinicia la historia de la síntesis: borra la salida reciente y la memoria LPC, y devuelve
    /// a sus valores iniciales el lag, el último índice de ganancia y el tipo de señal anterior.
    /// Conserva la ganancia anterior, la excitación y el estado de la ocultación y del ruido de
    /// confort. Es lo que libopus reinicia en el canal lateral cuando vuelve a codificarse tras
    /// una trama solo de medio (`silk_Decode`).
    pub(super) fn reset_synthesis_history(&mut self) {
        self.out_buf.fill(0);
        self.s_lpc_q14_buf.fill(0);
        self.lag_prev = INITIAL_LAG_PREV;
        self.last_gain_index = INITIAL_LAST_GAIN_INDEX;
        self.prev_signal_type = TYPE_NO_VOICE_ACTIVITY;
    }
}

impl Default for SilkChannelState {
    /// Estado a cero salvo los valores iniciales de libopus: ganancia anterior, último índice de
    /// ganancia y lag anterior.
    fn default() -> Self {
        Self {
            prev_gain_q16: ONE_Q16,
            last_gain_index: INITIAL_LAST_GAIN_INDEX,
            prev_signal_type: TYPE_NO_VOICE_ACTIVITY,
            lag_prev: INITIAL_LAG_PREV,
            s_lpc_q14_buf: [0; MAX_LPC_ORDER],
            out_buf: [0; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH],
            exc_q14: [0; MAX_FRAME_LENGTH],
            plc: SilkPlcState::default(),
            cng: CngState::default(),
        }
    }
}

/// Desplazamiento de un pulso entero a la excitación en Q14 (`silk_LSHIFT(pulses[i], 14)`).
const PULSE_Q14_SHIFT: u32 = 14;
/// Paso de Q10 a Q14, con el que se suman a la excitación el ajuste de nivel, el desplazamiento
/// del cuantificador y la predicción LPC.
const Q10_TO_Q14_SHIFT: u32 = 4;
/// Paso de la ganancia de Q16 a Q10 (`silk_RSHIFT(Gains_Q16[k], 6)`).
const GAIN_Q16_TO_Q10_SHIFT: u32 = 6;
/// Q del inverso de la ganancia: el de una ganancia en Q16 calculado en Q47 queda en Q31
/// (`silk_INVERSE32_varQ(Gains_Q16[k], 47)`).
const INV_GAIN_Q: usize = 47;
/// Q del cociente entre la ganancia anterior y la actual (`silk_DIV32_varQ(..., 16)`).
const GAIN_ADJ_Q: usize = 16;
/// `SMULWB` de la inversa en Q31 por la escala LTP en Q14 deja Q29; dos bits la devuelven a Q31.
const LTP_SCALE_Q29_TO_Q31_SHIFT: u32 = 2;
/// `SMULWW` de la señal en Q14 por la ganancia en Q10 da Q8, que se redondea a Q0.
const OUTPUT_Q8_SHIFT: usize = 8;

/// Filtro de tono de una trama: si se aplica en cada subtrama, lags, coeficientes y escala LTP.
/// En tramas no sonoras es todo cero y no se aplica, salvo en el paso de una ocultación sonora a
/// una trama no sonora (ver [`PitchFilter::smooth_after_voiced_loss`]).
struct PitchFilter {
    voiced: [bool; MAX_NB_SUBFR],
    pitch_lags: [i32; MAX_NB_SUBFR],
    ltp_coeffs_q14: [[i16; LTP_ORDER]; MAX_NB_SUBFR],
    ltp_scale_q14: i32,
}

impl PitchFilter {
    fn new(params: &SilkFrameParams) -> Self {
        if params.signal_type != TYPE_VOICED {
            return Self {
                voiced: [false; MAX_NB_SUBFR],
                pitch_lags: [0; MAX_NB_SUBFR],
                ltp_coeffs_q14: [[0; LTP_ORDER]; MAX_NB_SUBFR],
                ltp_scale_q14: 0,
            };
        }
        let pitch_lags = pitch::decode_pitch_lags(
            params.lag_index as i32,
            params.contour_index as i32,
            params.fs_khz as i32,
            params.nb_subfr,
        );
        let (ltp_coeffs_q14, ltp_scale_q14) = ltp::decode_ltp_coeffs(
            params.ltp_periodicity,
            &params.ltp_indices,
            params.nb_subfr,
            params.ltp_scale_index,
        );
        Self {
            voiced: [true; MAX_NB_SUBFR],
            pitch_lags,
            ltp_coeffs_q14,
            ltp_scale_q14,
        }
    }

    /// Tras ocultar tramas sonoras, la primera trama buena no sonora conserva el tono en sus dos
    /// primeras subtramas, con un solo coeficiente de 0,25 y el lag anterior, para evitar un salto
    /// brusco (`silk_decode_core`).
    fn smooth_after_voiced_loss(&mut self, params: &SilkFrameParams, state: &SilkChannelState) {
        let after_voiced_loss = state.plc.loss_cnt() > 0
            && state.prev_signal_type == TYPE_VOICED
            && params.signal_type != TYPE_VOICED;
        if !after_voiced_loss {
            return;
        }
        for k in 0..VOICED_TO_UNVOICED_SUBFRAMES.min(params.nb_subfr) {
            self.voiced[k] = true;
            self.pitch_lags[k] = state.lag_prev;
            self.ltp_coeffs_q14[k] = [0; LTP_ORDER];
            self.ltp_coeffs_q14[k][LTP_ORDER / 2] = VOICED_TO_UNVOICED_LTP_Q14;
        }
    }
}

/// Reconstruye en `exc_q14[..frame_length]` la excitación en Q14 a partir de los pulsos: cada
/// pulso se acerca a cero en `QUANT_LEVEL_ADJUST_Q10`, se le suma el desplazamiento del
/// cuantificador y su signo se invierte según el generador pseudoaleatorio (`silk_decode_core`).
/// El resto de `exc_q14` no se toca.
fn excitation(params: &SilkFrameParams, exc_q14: &mut [i32; MAX_FRAME_LENGTH]) {
    let offset_q10 = QUANTIZATION_OFFSETS_Q10[(params.signal_type >> 1) as usize]
        [params.quant_offset_type as usize];
    let mut rand_seed = params.seed as i32;
    for (exc, &pulse) in exc_q14
        .iter_mut()
        .zip(&params.pulses[..params.frame_length])
    {
        rand_seed = silk_rand(rand_seed);
        let mut sample = (pulse as i32) << PULSE_Q14_SHIFT;
        if sample > 0 {
            sample -= QUANT_LEVEL_ADJUST_Q10 << Q10_TO_Q14_SHIFT;
        } else if sample < 0 {
            sample += QUANT_LEVEL_ADJUST_Q10 << Q10_TO_Q14_SHIFT;
        }
        sample += offset_q10 << Q10_TO_Q14_SHIFT;
        if rand_seed < 0 {
            sample = -sample;
        }
        rand_seed = rand_seed.wrapping_add(pulse as i32);
        *exc = sample;
    }
}

/// Estado de trabajo de la síntesis de una trama: la memoria del filtro LPC y la historia del
/// filtro de tono, que se recorren subtrama a subtrama.
struct Synthesis<'a> {
    params: &'a SilkFrameParams,
    lpc_coeffs_q12: &'a [[i16; MAX_LPC_ORDER]; 2],
    filter: PitchFilter,
    subfr_length: usize,
    ltp_mem_length: usize,
    /// Las dos mitades de la trama tienen coeficientes LPC distintos.
    nlsf_interpolation: bool,
    s_lpc_q14: [i32; MAX_LPC_ORDER + MAX_SUB_FRAME_LENGTH],
    s_ltp: [i16; MAX_FRAME_LENGTH],
    ltp_history_q15: [i32; MAX_FRAME_LENGTH * 2],
    ltp_buf_idx: usize,
}

impl<'a> Synthesis<'a> {
    fn new(
        params: &'a SilkFrameParams,
        lpc_coeffs_q12: &'a [[i16; MAX_LPC_ORDER]; 2],
        state: &SilkChannelState,
    ) -> Self {
        let ltp_mem_length = params.fs_khz as usize * LTP_MEM_LENGTH_MS;
        let mut s_lpc_q14 = [0i32; MAX_LPC_ORDER + MAX_SUB_FRAME_LENGTH];
        s_lpc_q14[..MAX_LPC_ORDER].copy_from_slice(&state.s_lpc_q14_buf);
        let mut filter = PitchFilter::new(params);
        filter.smooth_after_voiced_loss(params, state);
        Self {
            params,
            lpc_coeffs_q12,
            filter,
            subfr_length: params.frame_length / params.nb_subfr,
            ltp_mem_length,
            nlsf_interpolation: lpc_coeffs_q12[0][..params.lpc_order]
                != lpc_coeffs_q12[1][..params.lpc_order],
            s_lpc_q14,
            s_ltp: [0; MAX_FRAME_LENGTH],
            ltp_history_q15: [0; MAX_FRAME_LENGTH * 2],
            ltp_buf_idx: ltp_mem_length,
        }
    }

    /// Sintetiza la subtrama `k` con ganancia `gain_q16` en `output_pcm`.
    fn subframe(
        &mut self,
        k: usize,
        gain_q16: i32,
        exc_q14: &[i32],
        output_pcm: &mut [i16],
        state: &mut SilkChannelState,
    ) {
        let gain_adj_q16 = self.rescale_lpc_memory(gain_q16, state);
        if self.filter.voiced[k] {
            self.update_ltp_history(k, gain_q16, gain_adj_q16, output_pcm, state);
        }
        let range = k * self.subfr_length..(k + 1) * self.subfr_length;
        let pres_q14 = self.ltp_residual(k, &exc_q14[range.clone()]);
        let gain_q10 = gain_q16 >> GAIN_Q16_TO_Q10_SHIFT;
        self.lpc_synthesis(k, &pres_q14, gain_q10, &mut output_pcm[range]);
    }

    /// Ajusta la memoria LPC al cambio de ganancia y devuelve el cociente en Q16 entre la
    /// ganancia anterior y `gain_q16` (uno si no cambia).
    fn rescale_lpc_memory(&mut self, gain_q16: i32, state: &mut SilkChannelState) -> i32 {
        let gain_adj_q16 = if gain_q16 != state.prev_gain_q16 {
            div32_var_q(state.prev_gain_q16, gain_q16, GAIN_ADJ_Q)
        } else {
            ONE_Q16
        };
        if gain_q16 != state.prev_gain_q16 {
            for sample in self.s_lpc_q14.iter_mut().take(MAX_LPC_ORDER) {
                *sample = smulww(gain_adj_q16, *sample);
            }
        }
        state.prev_gain_q16 = gain_q16;
        gain_adj_q16
    }

    /// Prepara la historia del filtro de tono de la subtrama `k`: en la primera subtrama, y en la
    /// tercera si las mitades tienen coeficientes LPC distintos, se reblanquea la salida anterior;
    /// en las demás, se reescala con el cambio de ganancia.
    fn update_ltp_history(
        &mut self,
        k: usize,
        gain_q16: i32,
        gain_adj_q16: i32,
        output_pcm: &[i16],
        state: &mut SilkChannelState,
    ) {
        let history_len = self.filter.pitch_lags[k] as usize + LTP_ORDER / 2;
        if k == 0 || (k == 2 && self.nlsf_interpolation) {
            self.rewhiten(k, output_pcm, state);
            let mut inv_gain_q31 = inverse32_var_q(gain_q16, INV_GAIN_Q);
            if k == 0 {
                inv_gain_q31 =
                    smulwb(inv_gain_q31, self.filter.ltp_scale_q14) << LTP_SCALE_Q29_TO_Q31_SHIFT;
            }
            for i in 0..history_len {
                self.ltp_history_q15[self.ltp_buf_idx - i - 1] =
                    smulwb(inv_gain_q31, self.s_ltp[self.ltp_mem_length - i - 1] as i32);
            }
        } else if gain_adj_q16 != ONE_Q16 {
            for i in 0..history_len {
                let idx = self.ltp_buf_idx - i - 1;
                self.ltp_history_q15[idx] = smulww(gain_adj_q16, self.ltp_history_q15[idx]);
            }
        }
    }

    /// Reblanquea con los coeficientes de la subtrama `k` la salida reciente guardada en el
    /// historial del canal; en la tercera subtrama, el historial incluye antes la primera mitad
    /// de esta trama.
    fn rewhiten(&mut self, k: usize, output_pcm: &[i16], state: &mut SilkChannelState) {
        let order = self.params.lpc_order;
        let mem = self.ltp_mem_length;
        let start_idx = mem - self.filter.pitch_lags[k] as usize - order - (LTP_ORDER / 2);
        if k == 2 {
            let copy_len = 2 * self.subfr_length;
            state.out_buf[mem..mem + copy_len].copy_from_slice(&output_pcm[..copy_len]);
        }
        let input_start = start_idx + k * self.subfr_length;
        lpc_analysis_filter(
            &mut self.s_ltp[start_idx..mem],
            &state.out_buf[input_start..input_start + (mem - start_idx)],
            &self.lpc_coeffs_q12[k >> 1],
            mem - start_idx,
            order,
        );
    }

    /// Residuo de la subtrama `k`: la excitación más, en tramas sonoras, la predicción de tono.
    fn ltp_residual(&mut self, k: usize, exc_subframe: &[i32]) -> [i32; MAX_SUB_FRAME_LENGTH] {
        let subfr_length = self.subfr_length;
        let mut pres_q14 = [0i32; MAX_SUB_FRAME_LENGTH];
        pres_q14[..subfr_length].copy_from_slice(exc_subframe);
        let pitch_lag = self.filter.pitch_lags[k];
        if self.filter.voiced[k] && pitch_lag > 0 {
            ltp::ltp_filter(
                &mut pres_q14[..subfr_length],
                exc_subframe,
                &mut self.ltp_history_q15,
                &mut self.ltp_buf_idx,
                pitch_lag as usize,
                subfr_length,
                &self.filter.ltp_coeffs_q14[k],
            );
        }
        pres_q14
    }

    /// Filtro de síntesis LPC de la subtrama `k` sobre el residuo `pres_q14`, con la salida
    /// escalada por `gain_q10` en `output`.
    fn lpc_synthesis(
        &mut self,
        k: usize,
        pres_q14: &[i32; MAX_SUB_FRAME_LENGTH],
        gain_q10: i32,
        output: &mut [i16],
    ) {
        let order = self.params.lpc_order;
        let a_q12 = &self.lpc_coeffs_q12[k >> 1][..order];
        for (i, (&pres, pcm)) in pres_q14.iter().zip(output.iter_mut()).enumerate() {
            let mut lpc_pred_q10 = (order as i32) >> 1;
            for (j, &a) in a_q12.iter().enumerate() {
                lpc_pred_q10 = smlawb(
                    lpc_pred_q10,
                    self.s_lpc_q14[MAX_LPC_ORDER + i - 1 - j],
                    a as i32,
                );
            }
            let current_q14 =
                add_sat32(pres, lshift_sat32(lpc_pred_q10, Q10_TO_Q14_SHIFT as usize));
            self.s_lpc_q14[MAX_LPC_ORDER + i] = current_q14;
            *pcm = sat16(rshift_round(smulww(current_q14, gain_q10), OUTPUT_Q8_SHIFT));
        }
        let subfr_length = self.subfr_length;
        self.s_lpc_q14
            .copy_within(subfr_length..subfr_length + MAX_LPC_ORDER, 0);
    }

    /// Guarda en el estado del canal la memoria LPC, el tipo de señal y la salida de la trama,
    /// para la siguiente trama y para la ocultación, y devuelve el control de la trama.
    fn finish(
        self,
        output_pcm: &[i16],
        gains_q16: [i32; MAX_NB_SUBFR],
        state: &mut SilkChannelState,
    ) -> FrameControl {
        let params = self.params;
        state
            .s_lpc_q14_buf
            .copy_from_slice(&self.s_lpc_q14[..MAX_LPC_ORDER]);
        state.prev_signal_type = params.signal_type;
        push_output(
            &mut state.out_buf,
            self.ltp_mem_length,
            &output_pcm[..params.frame_length],
        );
        // Como `silk_decode_parameters`, los coeficientes y la escala son los de los parámetros:
        // el suavizado tras una ocultación sonora solo deja su huella en el lag (`pitchL`).
        let decoded = PitchFilter::new(params);
        FrameControl {
            gains_q16,
            pitch_l: self.filter.pitch_lags,
            ltp_coef_q14: decoded.ltp_coeffs_q14,
            ltp_scale_q14: decoded.ltp_scale_q14,
        }
    }
}

/// Añade `frame` al final de la salida reciente `out_buf`, de `ltp_mem_length` muestras útiles,
/// desplazando hacia el principio lo que cabe de la anterior.
fn push_output(out_buf: &mut [i16], ltp_mem_length: usize, frame: &[i16]) {
    let frame_len = frame.len();
    let mv_len = ltp_mem_length.saturating_sub(frame_len);
    if mv_len > 0 {
        out_buf.copy_within(frame_len..frame_len + mv_len, 0);
    }
    out_buf[mv_len..mv_len + frame_len].copy_from_slice(frame);
}

/// Decodifica una trama interna de SILK a PCM a la frecuencia interna (`silk_decode_core`):
/// excitación, filtro de tono en tramas sonoras y filtro de síntesis LPC, subtrama a subtrama.
///
/// `lpc_coeffs_q12` son los coeficientes de cada mitad de la trama. Escribe la trama en
/// `output_pcm`, actualiza el estado del canal y devuelve el control de la trama.
pub(super) fn decode_core(
    params: &SilkFrameParams,
    lpc_coeffs_q12: &[[i16; MAX_LPC_ORDER]; 2],
    output_pcm: &mut [i16],
    state: &mut SilkChannelState,
) -> FrameControl {
    let gains_q16 = gain::decode_gains(
        &params.gain_indices[..params.nb_subfr],
        &mut state.last_gain_index,
        params.conditional,
        params.nb_subfr,
    );
    excitation(params, &mut state.exc_q14);
    let exc_q14 = state.exc_q14;
    let mut synthesis = Synthesis::new(params, lpc_coeffs_q12, state);
    for (k, &gain_q16) in gains_q16.iter().enumerate().take(params.nb_subfr) {
        synthesis.subframe(k, gain_q16, &exc_q14, output_pcm, state);
    }
    synthesis.finish(output_pcm, gains_q16, state)
}

#[cfg(test)]
mod tests {
    use super::{INITIAL_LAG_PREV, INITIAL_LAST_GAIN_INDEX, SilkChannelState};
    use crate::silk::tables::{LTP_MEM_LENGTH_MS, TYPE_NO_VOICE_ACTIVITY, TYPE_VOICED};

    /// Frecuencia interna de las pruebas, en kHz: la historia de tono es de 160 muestras.
    const FS_KHZ: u32 = 8;
    /// Trama ocultada de las pruebas: la mitad de la historia de tono.
    const FRAME_LEN: usize = 80;
    /// Lag de tono distinto del inicial.
    const SOME_LAG: i32 = 77;

    /// Estado con la salida reciente numerada (muestra `i` = `i`).
    fn numbered_state() -> SilkChannelState {
        let mut state = SilkChannelState::default();
        for (i, sample) in state.out_buf.iter_mut().enumerate() {
            *sample = i as i16;
        }
        state
    }

    /// La trama ocultada entra al final de la historia de tono, que avanza lo que dura la trama.
    #[test]
    fn concealed_frame_enters_history() {
        let mut state = numbered_state();
        let frame: Vec<i16> = (0..FRAME_LEN).map(|i| -(i as i16) - 1).collect();
        state.record_concealed_frame(&frame, FS_KHZ);

        let ltp_mem_length = FS_KHZ as usize * LTP_MEM_LENGTH_MS;
        let kept = ltp_mem_length - FRAME_LEN;
        for i in 0..kept {
            assert_eq!(state.out_buf[i], (i + FRAME_LEN) as i16, "muestra {i}");
        }
        assert_eq!(&state.out_buf[kept..ltp_mem_length], &frame[..]);
    }

    /// Ganancia anterior de la prueba de reinicio, distinta de la inicial (1,0 en Q16).
    const KEPT_GAIN_Q16: i32 = 12_345;

    /// El reinicio del canal lateral borra la salida reciente y la memoria LPC y devuelve el lag,
    /// el último índice de ganancia y el tipo de señal a sus valores iniciales; la ganancia
    /// anterior se conserva, como en `silk_Decode` de libopus.
    #[test]
    fn side_reset_after_mid_only_clears_synthesis_history() {
        let mut state = numbered_state();
        state.s_lpc_q14_buf.fill(-7);
        state.lag_prev = SOME_LAG;
        state.last_gain_index = INITIAL_LAST_GAIN_INDEX + 5;
        state.prev_signal_type = TYPE_VOICED;
        state.prev_gain_q16 = KEPT_GAIN_Q16;

        state.reset_synthesis_history();

        assert!(state.out_buf.iter().all(|&sample| sample == 0));
        assert!(state.s_lpc_q14_buf.iter().all(|&value| value == 0));
        assert_eq!(state.lag_prev, INITIAL_LAG_PREV);
        assert_eq!(state.last_gain_index, INITIAL_LAST_GAIN_INDEX);
        assert_eq!(state.prev_signal_type, TYPE_NO_VOICE_ACTIVITY);
        assert_eq!(state.prev_gain_q16, KEPT_GAIN_Q16);
    }

    /// Tras una pérdida, el último índice de ganancia vuelve al inicial.
    #[test]
    fn loss_forgets_last_gain_index() {
        let mut state = SilkChannelState {
            last_gain_index: INITIAL_LAST_GAIN_INDEX + 20,
            ..SilkChannelState::default()
        };
        state.forget_last_gain_after_loss();
        assert_eq!(state.last_gain_index, INITIAL_LAST_GAIN_INDEX);
    }
}

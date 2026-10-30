//! Ocultación de pérdidas de SILK, como `silk/PLC.c` de libopus 1.6.1.
//!
//! Tras cada trama interna decodificada se guarda lo que necesita la ocultación
//! ([`SilkPlcState::update`]): el tono y los coeficientes LTP de la última subtrama con pulso de
//! tono, los LPC, la escala LTP y las dos últimas ganancias. Una trama perdida se sintetiza
//! ([`SilkPlcState::conceal`]) excitando el filtro de tono con la historia reblanqueada de la
//! salida más ruido sacado de la excitación de la última trama buena, y el filtro LPC con esa
//! excitación; las ganancias se atenúan trama a trama y el tono se alarga despacio. La primera
//! trama buena tras una pérdida se funde con la energía de la ocultación
//! ([`SilkPlcState::glue_frames`]).

use super::decode_core::FrameControl;
use super::fixed::{
    add_sat32, inverse32_var_q, lshift_sat32, rshift_round, sat16, silk_rand, smlawb, smulbb,
    smulwb, smulww, sqrt_approx,
};
use super::lpc::{bwexpander, lpc_analysis_filter, lpc_inv_pred_gain};
use super::tables::{
    LTP_MEM_LENGTH_MS, LTP_ORDER, MAX_FRAME_LENGTH, MAX_LPC_ORDER, MAX_SUB_FRAME_LENGTH, ONE_Q16,
    TYPE_VOICED,
};

/// Escalones de atenuación: la primera trama ocultada usa el primero; las demás, el segundo.
const NB_ATT: usize = 2;
/// Atenuación del filtro de tono por subtrama ocultada: 0,99 y 0,95 en Q15 (`HARM_ATT_Q15`).
const HARM_ATT_Q15: [i32; NB_ATT] = [32_440, 31_130];
/// Atenuación del ruido por subtrama tras una trama sonora: 0,95 y 0,8 en Q15.
const PLC_RAND_ATTENUATE_V_Q15: [i32; NB_ATT] = [31_130, 26_214];
/// Atenuación del ruido por subtrama tras una trama no sonora: 0,99 y 0,9 en Q15.
const PLC_RAND_ATTENUATE_UV_Q15: [i32; NB_ATT] = [32_440, 29_491];
/// Expansión de ancho de banda de los LPC en cada trama ocultada: 0,99 en Q16
/// (`SILK_FIX_CONST(BWE_COEF, 16)`).
const BWE_COEF_Q16: i32 = 64_881;
/// Límites de la ganancia LTP al guardarla: 0,7 y 0,95 en Q14 (`V_PITCH_GAIN_START_*_Q14`).
const V_PITCH_GAIN_START_MIN_Q14: i32 = 11_469;
const V_PITCH_GAIN_START_MAX_Q14: i32 = 15_565;
/// Lag de tono máximo de la ocultación, en milisegundos.
const MAX_PITCH_LAG_MS: i32 = 18;
/// Lag de tono de una trama no sonora, en milisegundos: 18 por kHz (`silk_SMULBB(fs_kHz, 18)`).
const UNVOICED_PITCH_LAG_MS: i32 = 18;
/// Muestras de la excitación de la que se saca el ruido (`RAND_BUF_SIZE`), potencia de dos.
const RAND_BUF_SIZE: usize = 128;
const RAND_BUF_MASK: i32 = RAND_BUF_SIZE as i32 - 1;
/// Desplazamiento que deja en el rango de `RAND_BUF_MASK` el estado del generador.
const RAND_INDEX_SHIFT: u32 = 25;
/// Alargamiento del lag de tono por subtrama ocultada: 0,01 en Q16.
const PITCH_DRIFT_FAC_Q16: i32 = 655;
/// Umbrales de la ganancia LPC por debajo y por encima de los que no se reduce más el ruido
/// tras una trama no sonora: 2^3 (8 dB) y 2^8 (24 dB), en log2 (`LOG2_INV_LPC_GAIN_*_THRES`).
const LOG2_INV_LPC_GAIN_HIGH_THRES: u32 = 3;
const LOG2_INV_LPC_GAIN_LOW_THRES: u32 = 8;
/// Escala mínima del ruido en la primera trama ocultada tras una sonora: 0,2 en Q14.
const RAND_SCALE_MIN_Q14: i32 = 3_277;
/// 1,0 en Q14, la escala inicial del ruido.
const ONE_Q14: i32 = 1 << 14;
/// Q del inverso de la ganancia con que se escala la historia de tono (`silk_INVERSE32_varQ(...,
/// 46)`), que deja la historia en Q14.
const INV_GAIN_Q: usize = 46;
/// Paso de Q10 a Q14 de la predicción LPC.
const Q10_TO_Q14_SHIFT: usize = 4;
/// `SMULWW` de la señal en Q14 por la ganancia en Q10 da Q8, que se redondea a Q0.
const OUTPUT_Q8_SHIFT: usize = 8;
/// Paso de la ganancia de Q16 a Q10.
const GAIN_Q16_TO_Q10_SHIFT: u32 = 6;
/// Valores de [`SilkPlcState::reset`] (`silk_PLC_Reset`): subtramas de 20 muestras, dos por
/// trama, y el lag a la mitad de la trama, en Q8.
const RESET_SUBFR_LENGTH: usize = 20;
const RESET_NB_SUBFR: usize = 2;
const RESET_PITCH_L_SHIFT: u32 = 8 - 1;
/// Pendiente del fundido de la primera trama buena tras una pérdida, cuatro veces más rápida
/// para no perder ataques tras DTX (`silk_PLC_glue_frames`).
const GLUE_SLOPE_SHIFT: u32 = 2;
/// Q de la relación de energías del fundido.
const GLUE_FRAC_Q: i32 = 24;

/// Estado de la ocultación de pérdidas de un canal de SILK (`silk_PLC_struct` más el contador de
/// pérdidas `lossCnt`).
#[derive(Debug, Clone)]
pub(super) struct SilkPlcState {
    /// Lag de tono, en Q8, a la frecuencia interna.
    pitch_l_q8: i32,
    /// Coeficientes LTP de la ocultación, en Q14; se atenúan en cada subtrama ocultada.
    ltp_coef_q14: [i16; LTP_ORDER],
    /// Coeficientes LPC de la última trama buena, en Q12; se expanden en cada trama ocultada.
    prev_lpc_q12: [i16; MAX_LPC_ORDER],
    /// La trama anterior se ocultó.
    last_frame_lost: bool,
    /// Semilla del generador con que se elige el ruido.
    rand_seed: i32,
    /// Escala del ruido, en Q14.
    rand_scale_q14: i16,
    /// Energía de la última trama ocultada y su desplazamiento, para el fundido.
    conc_energy: i32,
    conc_energy_shift: i32,
    /// Escala LTP de la última trama buena, en Q14.
    prev_ltp_scale_q14: i16,
    /// Ganancias de las dos últimas subtramas de la última trama buena, en Q16.
    prev_gain_q16: [i32; 2],
    /// Frecuencia interna para la que vale el estado, en kHz; 0 antes de la primera trama.
    fs_khz: u32,
    /// Subtramas y muestras por subtrama de la última trama buena.
    nb_subfr: usize,
    subfr_length: usize,
    /// Tramas internas ocultadas seguidas (`lossCnt`); vuelve a 0 con cada trama buena.
    loss_cnt: u32,
}

impl Default for SilkPlcState {
    /// Estado de un decodificador recién creado: todo a cero salvo lo que fija `silk_PLC_Reset`
    /// con una trama de longitud 0, como `silk_init_decoder`.
    fn default() -> Self {
        let mut state = Self {
            pitch_l_q8: 0,
            ltp_coef_q14: [0; LTP_ORDER],
            prev_lpc_q12: [0; MAX_LPC_ORDER],
            last_frame_lost: false,
            rand_seed: 0,
            rand_scale_q14: 0,
            conc_energy: 0,
            conc_energy_shift: 0,
            prev_ltp_scale_q14: 0,
            prev_gain_q16: [ONE_Q16; 2],
            fs_khz: 0,
            nb_subfr: RESET_NB_SUBFR,
            subfr_length: RESET_SUBFR_LENGTH,
            loss_cnt: 0,
        };
        state.reset(0);
        state
    }
}

/// Lo que necesita la ocultación del estado de síntesis del canal (`silk_decoder_state`).
pub(super) struct ConcealInput<'a> {
    /// Excitación de la última trama buena, en Q14, con lo que quede de las anteriores detrás.
    pub exc_q14: &'a [i32; MAX_FRAME_LENGTH],
    /// Salida reciente del canal (`outBuf`).
    pub out_buf: &'a [i16],
    /// Tipo de señal de la última trama buena.
    pub prev_signal_type: i32,
    /// No se ha decodificado ninguna trama desde el último cambio de frecuencia.
    pub first_frame_after_reset: bool,
    /// Frecuencia interna, en kHz, y orden LPC.
    pub fs_khz: u32,
    pub lpc_order: usize,
    /// Subtramas y muestras por subtrama de la trama que se oculta.
    pub nb_subfr: usize,
    pub subfr_length: usize,
}

/// Lo que guarda la ocultación de una trama decodificada (`silk_PLC_update`).
pub(super) struct DecodedFrame<'a> {
    /// Tipo de señal de la trama.
    pub signal_type: i32,
    /// Ganancias, tono y coeficientes LTP de la trama.
    pub control: &'a FrameControl,
    /// Coeficientes LPC de la segunda mitad de la trama, en Q12, de su orden.
    pub pred_coef_q12: &'a [i16],
    /// Subtramas y muestras por subtrama de la trama, y frecuencia interna en kHz.
    pub nb_subfr: usize,
    pub subfr_length: usize,
    pub fs_khz: u32,
}

impl SilkPlcState {
    /// Tramas internas ocultadas seguidas.
    pub(super) fn loss_cnt(&self) -> u32 {
        self.loss_cnt
    }

    /// Escala del ruido, en Q14, y ganancia de la última subtrama buena, en Q16: con ellas el
    /// ruido de confort descuenta la energía que ya pone la ocultación.
    pub(super) fn noise_gain_inputs(&self) -> (i32, i32) {
        (i32::from(self.rand_scale_q14), self.prev_gain_q16[1])
    }

    /// Vuelve a los valores iniciales de `silk_PLC_Reset` para tramas de `frame_length` muestras.
    fn reset(&mut self, frame_length: usize) {
        self.pitch_l_q8 = (frame_length as i32) << RESET_PITCH_L_SHIFT;
        self.prev_gain_q16 = [ONE_Q16; 2];
        self.subfr_length = RESET_SUBFR_LENGTH;
        self.nb_subfr = RESET_NB_SUBFR;
    }

    /// Reinicia el estado si cambia la frecuencia interna (`silk_PLC`, al entrar).
    fn ensure_rate(&mut self, fs_khz: u32, frame_length: usize) {
        if fs_khz != self.fs_khz {
            self.reset(frame_length);
            self.fs_khz = fs_khz;
        }
    }

    /// Guarda lo que necesita la ocultación de la trama decodificada `frame` y pone a cero las
    /// pérdidas seguidas (`silk_PLC` sin pérdida y `silk_decode_frame`).
    pub(super) fn update(&mut self, frame: &DecodedFrame<'_>) {
        self.ensure_rate(frame.fs_khz, frame.nb_subfr * frame.subfr_length);
        let control = frame.control;
        let nb_subfr = frame.nb_subfr;
        if frame.signal_type == TYPE_VOICED {
            self.update_voiced(control, nb_subfr, frame.subfr_length);
        } else {
            self.pitch_l_q8 = smulbb(frame.fs_khz as i32, UNVOICED_PITCH_LAG_MS) << 8;
            self.ltp_coef_q14 = [0; LTP_ORDER];
        }
        let order = frame.pred_coef_q12.len();
        self.prev_lpc_q12[..order].copy_from_slice(frame.pred_coef_q12);
        self.prev_ltp_scale_q14 = control.ltp_scale_q14 as i16;
        self.prev_gain_q16 = [
            control.gains_q16[nb_subfr - 2],
            control.gains_q16[nb_subfr - 1],
        ];
        self.subfr_length = frame.subfr_length;
        self.nb_subfr = nb_subfr;
        self.loss_cnt = 0;
    }

    /// Tono y coeficientes LTP de una trama sonora: los de la subtrama con más ganancia LTP entre
    /// las que contienen el último pulso de tono, con la ganancia llevada a `0,7..=0,95`.
    fn update_voiced(&mut self, control: &FrameControl, nb_subfr: usize, subfr_length: usize) {
        let last_lag = control.pitch_l[nb_subfr - 1];
        let mut ltp_gain_q14 = 0i32;
        let mut j = 0usize;
        while (j * subfr_length) < last_lag as usize && j < nb_subfr {
            let subfr = nb_subfr - 1 - j;
            let gain: i32 = control.ltp_coef_q14[subfr]
                .iter()
                .map(|&c| i32::from(c))
                .sum();
            if gain > ltp_gain_q14 {
                // libopus copia aquí también los coeficientes, pero los sobrescribe al salir.
                ltp_gain_q14 = gain;
                self.pitch_l_q8 = control.pitch_l[subfr] << 8;
            }
            j += 1;
        }
        self.ltp_coef_q14 = [0; LTP_ORDER];
        self.ltp_coef_q14[LTP_ORDER / 2] = ltp_gain_q14 as i16;
        let (scale, shift) = if ltp_gain_q14 < V_PITCH_GAIN_START_MIN_Q14 {
            ((V_PITCH_GAIN_START_MIN_Q14 << 10) / ltp_gain_q14.max(1), 10)
        } else if ltp_gain_q14 > V_PITCH_GAIN_START_MAX_Q14 {
            ((V_PITCH_GAIN_START_MAX_Q14 << 14) / ltp_gain_q14.max(1), 14)
        } else {
            return;
        };
        for coef in &mut self.ltp_coef_q14 {
            *coef = (smulbb(i32::from(*coef), scale) >> shift) as i16;
        }
    }

    /// Sintetiza en `frame` una trama interna perdida (`silk_PLC_conceal`) y cuenta la pérdida.
    /// Usa y actualiza la memoria LPC del canal `s_lpc_q14_buf`. Devuelve el lag de tono con que
    /// termina, que pasa a ser el de la trama (`pitchL`). Si el lag no deja historia suficiente
    /// (no ocurre con tramas válidas), la trama queda a cero.
    pub(super) fn conceal(
        &mut self,
        input: &ConcealInput<'_>,
        s_lpc_q14_buf: &mut [i32; MAX_LPC_ORDER],
        frame: &mut [i16],
    ) -> i32 {
        let frame_length = input.nb_subfr * input.subfr_length;
        self.ensure_rate(input.fs_khz, frame_length);
        let lag = self.synthesize(input, s_lpc_q14_buf, &mut frame[..frame_length]);
        self.loss_cnt = self.loss_cnt.saturating_add(1);
        lag
    }

    fn synthesize(
        &mut self,
        input: &ConcealInput<'_>,
        s_lpc_q14_buf: &mut [i32; MAX_LPC_ORDER],
        frame: &mut [i16],
    ) -> i32 {
        let order = input.lpc_order;
        let ltp_mem_length = input.fs_khz as usize * LTP_MEM_LENGTH_MS;
        let prev_gain_q10 = self.prev_gain_q16.map(|g| g >> GAIN_Q16_TO_Q10_SHIFT);
        if input.first_frame_after_reset {
            self.prev_lpc_q12 = [0; MAX_LPC_ORDER];
        }
        let rand_offset = self.noise_offset(input, prev_gain_q10);
        let att = (self.loss_cnt as usize).min(NB_ATT - 1);
        let harm_gain_q15 = HARM_ATT_Q15[att];
        let voiced = input.prev_signal_type == TYPE_VOICED;
        let mut rand_gain_q15 = if voiced {
            PLC_RAND_ATTENUATE_V_Q15[att]
        } else {
            PLC_RAND_ATTENUATE_UV_Q15[att]
        };
        bwexpander(&mut self.prev_lpc_q12, order, BWE_COEF_Q16);
        let a_q12 = self.prev_lpc_q12;
        let mut rand_scale_q14 = i32::from(self.rand_scale_q14);
        if self.loss_cnt == 0 {
            (rand_scale_q14, rand_gain_q15) = self.first_loss_noise(order, voiced, rand_gain_q15);
        }

        let mut lag = rshift_round(self.pitch_l_q8, 8);
        let Some(idx) = ltp_mem_length
            .checked_sub(lag as usize + order + LTP_ORDER / 2)
            .filter(|&idx| idx > 0 && lag > 0)
        else {
            frame.fill(0);
            return lag;
        };
        // Historia de tono reblanqueada con los LPC expandidos y escalada por el inverso de la
        // última ganancia, en Q14; detrás se escribe la excitación de la trama.
        let mut s_ltp = [0i16; MAX_FRAME_LENGTH];
        lpc_analysis_filter(
            &mut s_ltp[idx..ltp_mem_length],
            &input.out_buf[idx..ltp_mem_length],
            &a_q12,
            ltp_mem_length - idx,
            order,
        );
        let inv_gain_q30 = inverse32_var_q(self.prev_gain_q16[1], INV_GAIN_Q).min(i32::MAX >> 1);
        let mut s_ltp_q14 = [0i32; 2 * MAX_FRAME_LENGTH];
        for i in idx + order..ltp_mem_length {
            s_ltp_q14[i] = smulwb(inv_gain_q30, i32::from(s_ltp[i]));
        }

        let mut rand_seed = self.rand_seed;
        let mut b_q14 = self.ltp_coef_q14.map(i32::from);
        let mut buf_idx = ltp_mem_length;
        for _ in 0..input.nb_subfr {
            let first_pred_idx = buf_idx - lag as usize + LTP_ORDER / 2;
            for pred_idx in first_pred_idx..first_pred_idx + input.subfr_length {
                let mut ltp_pred_q12 = 2;
                for (j, &b) in b_q14.iter().enumerate() {
                    ltp_pred_q12 = smlawb(ltp_pred_q12, s_ltp_q14[pred_idx - j], b);
                }
                rand_seed = silk_rand(rand_seed);
                let noise_idx = ((rand_seed >> RAND_INDEX_SHIFT) & RAND_BUF_MASK) as usize;
                let noise = input.exc_q14[rand_offset + noise_idx];
                s_ltp_q14[buf_idx] = smlawb(ltp_pred_q12, noise, rand_scale_q14).wrapping_shl(2);
                buf_idx += 1;
            }
            for b in &mut b_q14 {
                *b = (smulbb(harm_gain_q15, *b) >> 15) as i16 as i32;
            }
            rand_scale_q14 = (smulbb(rand_scale_q14, rand_gain_q15) >> 15) as i16 as i32;
            self.pitch_l_q8 = smlawb(self.pitch_l_q8, self.pitch_l_q8, PITCH_DRIFT_FAC_Q16)
                .min(smulbb(MAX_PITCH_LAG_MS, input.fs_khz as i32) << 8);
            lag = rshift_round(self.pitch_l_q8, 8);
        }
        self.ltp_coef_q14 = b_q14.map(|b| b as i16);

        // Filtro de síntesis LPC sobre la excitación, desde la memoria LPC del canal.
        let s_lpc = &mut s_ltp_q14[ltp_mem_length - MAX_LPC_ORDER..];
        s_lpc[..MAX_LPC_ORDER].copy_from_slice(s_lpc_q14_buf);
        for (i, sample) in frame.iter_mut().enumerate() {
            let mut lpc_pred_q10 = (order as i32) >> 1;
            for (j, &a) in a_q12[..order].iter().enumerate() {
                lpc_pred_q10 = smlawb(lpc_pred_q10, s_lpc[MAX_LPC_ORDER + i - j - 1], i32::from(a));
            }
            let current = add_sat32(
                s_lpc[MAX_LPC_ORDER + i],
                lshift_sat32(lpc_pred_q10, Q10_TO_Q14_SHIFT),
            );
            s_lpc[MAX_LPC_ORDER + i] = current;
            *sample = sat16(rshift_round(
                smulww(current, prev_gain_q10[1]),
                OUTPUT_Q8_SHIFT,
            ));
        }
        let frame_length = frame.len();
        s_lpc_q14_buf.copy_from_slice(&s_lpc[frame_length..frame_length + MAX_LPC_ORDER]);

        self.rand_seed = rand_seed;
        self.rand_scale_q14 = rand_scale_q14 as i16;
        lag
    }

    /// Posición en la excitación de la última trama buena de donde se saca el ruido: el final de
    /// la penúltima o de la última subtrama, la de menos energía (`silk_PLC_energy`).
    fn noise_offset(&self, input: &ConcealInput<'_>, prev_gain_q10: [i32; 2]) -> usize {
        let subfr_length = input.subfr_length;
        let mut scaled = [0i16; 2 * MAX_SUB_FRAME_LENGTH];
        for (k, gain_q10) in prev_gain_q10.iter().enumerate() {
            let start = (k + input.nb_subfr - 2) * subfr_length;
            for i in 0..subfr_length {
                scaled[k * subfr_length + i] =
                    sat16(smulww(input.exc_q14[start + i], *gain_q10) >> 8);
            }
        }
        let (energy1, shift1) = sum_sqr_shift(&scaled[..subfr_length]);
        let (energy2, shift2) = sum_sqr_shift(&scaled[subfr_length..2 * subfr_length]);
        let end_subfr = if (energy1 >> shift2) < (energy2 >> shift1) {
            self.nb_subfr - 1
        } else {
            self.nb_subfr
        };
        (end_subfr * self.subfr_length).saturating_sub(RAND_BUF_SIZE)
    }

    /// Escala del ruido y su atenuación en la primera trama ocultada: tras una sonora, el ruido
    /// se reduce lo que aporta el tono (al menos a 0,2) y se escala con la escala LTP; tras una
    /// no sonora, la atenuación se reduce si la ganancia LPC es alta.
    fn first_loss_noise(&self, order: usize, voiced: bool, rand_gain_q15: i32) -> (i32, i32) {
        if voiced {
            let mut scale = ONE_Q14;
            for &b in &self.ltp_coef_q14 {
                scale -= i32::from(b);
            }
            let scale = (scale as i16).max(RAND_SCALE_MIN_Q14 as i16);
            let scale = (smulbb(i32::from(scale), i32::from(self.prev_ltp_scale_q14)) >> 14) as i16;
            return (i32::from(scale), rand_gain_q15);
        }
        let inv_gain_q30 = lpc_inv_pred_gain(&self.prev_lpc_q12, order);
        // `silk_min_32` con el umbral alto y después `silk_max_32` con el bajo, que es menor.
        let down_scale_q30 = inv_gain_q30.clamp(
            (1i32 << 30) >> LOG2_INV_LPC_GAIN_LOW_THRES,
            (1i32 << 30) >> LOG2_INV_LPC_GAIN_HIGH_THRES,
        ) << LOG2_INV_LPC_GAIN_HIGH_THRES;
        (ONE_Q14, smulwb(down_scale_q30, rand_gain_q15) >> 14)
    }

    /// Funde una trama con la ocultación (`silk_PLC_glue_frames`): si es ocultada, guarda su
    /// energía; si es la primera buena tras una pérdida y tiene más energía que la ocultada,
    /// empieza con la ganancia que iguala las energías y sube hasta 1.
    pub(super) fn glue_frames(&mut self, frame: &mut [i16]) {
        if self.loss_cnt > 0 {
            (self.conc_energy, self.conc_energy_shift) = sum_sqr_shift(frame);
            self.last_frame_lost = true;
            return;
        }
        if self.last_frame_lost {
            self.fade_in(frame);
        }
        self.last_frame_lost = false;
    }

    fn fade_in(&mut self, frame: &mut [i16]) {
        let (mut energy, energy_shift) = sum_sqr_shift(frame);
        if energy_shift > self.conc_energy_shift {
            self.conc_energy >>= energy_shift - self.conc_energy_shift;
        } else if energy_shift < self.conc_energy_shift {
            energy >>= self.conc_energy_shift - energy_shift;
        }
        if energy <= self.conc_energy {
            return;
        }
        let lz = self.conc_energy.leading_zeros() as i32 - 1;
        self.conc_energy <<= lz;
        energy >>= (GLUE_FRAC_Q - lz).max(0);
        let frac_q24 = self.conc_energy / energy.max(1);
        let mut gain_q16 = sqrt_approx(frac_q24) << 4;
        let slope_q16 = ((ONE_Q16 - gain_q16) / frame.len() as i32) << GLUE_SLOPE_SHIFT;
        for sample in frame.iter_mut() {
            *sample = smulwb(gain_q16, i32::from(*sample)) as i16;
            gain_q16 += slope_q16;
            if gain_q16 > ONE_Q16 {
                break;
            }
        }
    }
}

/// Energía de `x` desplazada a la derecha lo justo para dejar dos bits de margen en 32 bits, y
/// ese desplazamiento (`silk_sum_sqr_shift`).
fn sum_sqr_shift(x: &[i16]) -> (i32, i32) {
    let len = x.len();
    let first_shift = 31 - (len as u32).leading_zeros() as i32;
    let pass =
        |shift: i32, start: i32| -> i32 {
            let mut nrg = start;
            let mut pairs = x.chunks_exact(2);
            for pair in &mut pairs {
                let tmp = (smulbb(i32::from(pair[0]), i32::from(pair[0])) as u32)
                    .wrapping_add(smulbb(i32::from(pair[1]), i32::from(pair[1])) as u32);
                nrg = (nrg as u32).wrapping_add(tmp >> shift) as i32;
            }
            if let [last] = pairs.remainder() {
                let tmp = smulbb(i32::from(*last), i32::from(*last)) as u32;
                nrg = (nrg as u32).wrapping_add(tmp >> shift) as i32;
            }
            nrg
        };
    let nrg = pass(first_shift, len as i32);
    let shift = (first_shift + 3 - nrg.leading_zeros() as i32).max(0);
    (pass(shift, 0), shift)
}

#[cfg(test)]
mod tests {
    use super::sum_sqr_shift;

    /// Casos calculados a mano con el algoritmo de `silk_sum_sqr_shift`: con poca energía no se
    /// desplaza (3² + 4² = 25); con la máxima, el resultado deja dos bits libres.
    #[test]
    fn sum_sqr_shift_matches_hand_computed_cases() {
        assert_eq!(sum_sqr_shift(&[3, 4]), (25, 0));
        assert_eq!(sum_sqr_shift(&[]), (0, 0));
        let loud = [i16::MAX; 320];
        let (nrg, shift) = sum_sqr_shift(&loud);
        assert!(nrg > 0 && nrg.leading_zeros() >= 2, "{nrg} << {shift}");
        let exact = 320u64 * (i16::MAX as u64).pow(2);
        assert!(((nrg as u64) << shift) <= exact && exact < ((nrg as u64 + 320) << shift));
    }
}

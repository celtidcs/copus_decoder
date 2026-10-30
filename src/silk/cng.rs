//! Ruido de confort de SILK, como `silk/CNG.c` de libopus 1.6.1.
//!
//! Con cada trama buena sin actividad de voz se suavizan las NLSF y la ganancia y se guarda la
//! excitación de la subtrama de más ganancia. Mientras se ocultan pérdidas (y durante la DTX, que
//! llega como pérdidas), se suma a la trama ruido sacado de esa excitación, filtrado con las NLSF
//! suavizadas y con la ganancia que no aporta ya la ocultación.

use super::fixed::{
    add_sat32, lshift_sat32, rshift_round, sat16, silk_rand, smlawb, smulwb, smulww, sqrt_approx,
};
use super::lpc::nlsf2a;
use super::tables::{self, MAX_FRAME_LENGTH, MAX_LPC_ORDER, TYPE_NO_VOICE_ACTIVITY};

/// Semilla inicial del generador (`silk_CNG_Reset`).
const INITIAL_RAND_SEED: i32 = 3_176_576;
/// Máscara máxima de la excitación de la que se saca el ruido: `2^floor(log2(MAX_FRAME_LENGTH)) - 1`.
const CNG_BUF_MASK_MAX: usize = 255;
/// Suavizado de la ganancia por subtrama: 0,25^(1/4) en Q16.
const CNG_GAIN_SMTH_Q16: i32 = 4_634;
/// Si la ganancia suavizada supera en 3 dB la de la subtrama, se toma la de la subtrama: -3 dB en
/// Q16.
const CNG_GAIN_SMTH_THRESHOLD_Q16: i32 = 46_396;
/// Suavizado de las NLSF por trama: 0,25 en Q16.
const CNG_NLSF_SMTH_Q16: i32 = 16_348;
/// Umbrales a partir de los que la ganancia se calcula con los 16 bits altos para no desbordar:
/// 2^21 para la de la ocultación y 2^23 para la suavizada, en Q16.
const LARGE_PLC_GAIN_Q16: i32 = 1 << 21;
const LARGE_SMTH_GAIN_Q16: i32 = 1 << 23;
/// La energía de la ocultación se resta multiplicada por 2^5 (`silk_SUB_LSHIFT32(..., 5)`).
const PLC_ENERGY_SHIFT: u32 = 5;
/// Desplazamiento que deja en el rango de la máscara el estado del generador.
const RAND_INDEX_SHIFT: u32 = 24;
/// Paso de Q10 a Q14 de la predicción LPC y de Q16 a Q10 de la ganancia.
const Q10_TO_Q14_SHIFT: usize = 4;
const GAIN_Q16_TO_Q10_SHIFT: u32 = 6;
/// `SMULWW` de la señal en Q14 por la ganancia en Q10 da Q8, que se redondea a Q0.
const OUTPUT_Q8_SHIFT: usize = 8;

/// Estado del ruido de confort de un canal (`silk_CNG_struct`).
#[derive(Debug, Clone)]
pub(super) struct CngState {
    /// Excitación guardada de las tramas sin actividad de voz, en Q14.
    exc_buf_q14: [i32; MAX_FRAME_LENGTH],
    /// NLSF suavizadas, en Q15.
    smth_nlsf_q15: [i16; MAX_LPC_ORDER],
    /// Memoria del filtro de síntesis, en Q14.
    synth_state: [i32; MAX_LPC_ORDER],
    /// Ganancia suavizada, en Q16.
    smth_gain_q16: i32,
    /// Semilla del generador.
    rand_seed: i32,
    /// Frecuencia interna para la que vale el estado, en kHz; 0 antes de la primera trama.
    fs_khz: u32,
}

impl Default for CngState {
    /// Estado de un decodificador recién creado: `silk_CNG_Reset` con orden LPC 0, como
    /// `silk_init_decoder`.
    fn default() -> Self {
        Self {
            exc_buf_q14: [0; MAX_FRAME_LENGTH],
            smth_nlsf_q15: [0; MAX_LPC_ORDER],
            synth_state: [0; MAX_LPC_ORDER],
            smth_gain_q16: 0,
            rand_seed: INITIAL_RAND_SEED,
            fs_khz: 0,
        }
    }
}

/// Lo que necesita el ruido de confort de la trama y del canal (`silk_decoder_state` y
/// `silk_decoder_control`).
pub(super) struct CngInput<'a> {
    /// Frecuencia interna, en kHz, y orden LPC.
    pub fs_khz: u32,
    pub lpc_order: usize,
    /// Tramas ocultadas seguidas: 0 si la trama es buena.
    pub loss_cnt: u32,
    /// Tipo de señal de la última trama buena (la actual, si es buena).
    pub prev_signal_type: i32,
    /// NLSF de la última trama buena, en Q15.
    pub prev_nlsf_q15: &'a [i16; MAX_LPC_ORDER],
    /// Ganancias de las subtramas de la trama buena, en Q16 (no se usan si la trama se oculta).
    pub gains_q16: &'a [i32],
    /// Subtramas y muestras por subtrama de la trama.
    pub nb_subfr: usize,
    pub subfr_length: usize,
    /// Excitación de la trama buena, en Q14.
    pub exc_q14: &'a [i32; MAX_FRAME_LENGTH],
    /// Escala del ruido de la ocultación, en Q14, y última ganancia buena, en Q16.
    pub plc_rand_scale_q14: i32,
    pub plc_prev_gain_q16: i32,
}

impl CngState {
    /// Vuelve a los valores iniciales para el orden LPC `lpc_order`: NLSF repartidas por igual,
    /// ganancia 0 y la semilla inicial (`silk_CNG_Reset`). Conserva la excitación y la memoria.
    fn reset(&mut self, lpc_order: usize) {
        let step_q15 = i32::from(i16::MAX) / (lpc_order as i32 + 1);
        let mut acc_q15 = 0;
        for nlsf in self.smth_nlsf_q15.iter_mut().take(lpc_order) {
            acc_q15 += step_q15;
            *nlsf = acc_q15 as i16;
        }
        self.smth_gain_q16 = 0;
        self.rand_seed = INITIAL_RAND_SEED;
    }

    /// Actualiza el ruido de confort con la trama `frame` y, si se está ocultando, se lo suma
    /// (`silk_CNG`).
    pub(super) fn apply(&mut self, input: &CngInput<'_>, frame: &mut [i16]) {
        if input.fs_khz != self.fs_khz {
            self.reset(input.lpc_order);
            self.fs_khz = input.fs_khz;
        }
        if input.loss_cnt == 0 && input.prev_signal_type == TYPE_NO_VOICE_ACTIVITY {
            self.update(input);
        }
        if input.loss_cnt > 0 {
            self.add_noise(input, frame);
        } else {
            self.synth_state[..input.lpc_order].fill(0);
        }
    }

    /// Suaviza las NLSF y la ganancia con una trama buena sin actividad de voz y guarda la
    /// excitación de su subtrama de más ganancia delante de la ya guardada.
    fn update(&mut self, input: &CngInput<'_>) {
        for (smth, &prev) in self
            .smth_nlsf_q15
            .iter_mut()
            .zip(input.prev_nlsf_q15)
            .take(input.lpc_order)
        {
            *smth = (i32::from(*smth)
                + smulwb(i32::from(prev) - i32::from(*smth), CNG_NLSF_SMTH_Q16))
                as i16;
        }
        let gains = &input.gains_q16[..input.nb_subfr];
        let mut max_gain_q16 = 0;
        let mut loudest = 0;
        for (i, &gain) in gains.iter().enumerate() {
            if gain > max_gain_q16 {
                max_gain_q16 = gain;
                loudest = i;
            }
        }
        let subfr_length = input.subfr_length;
        self.exc_buf_q14
            .copy_within(0..(input.nb_subfr - 1) * subfr_length, subfr_length);
        self.exc_buf_q14[..subfr_length]
            .copy_from_slice(&input.exc_q14[loudest * subfr_length..(loudest + 1) * subfr_length]);
        for &gain in gains {
            self.smth_gain_q16 += smulwb(gain - self.smth_gain_q16, CNG_GAIN_SMTH_Q16);
            if smulww(self.smth_gain_q16, CNG_GAIN_SMTH_THRESHOLD_Q16) > gain {
                self.smth_gain_q16 = gain;
            }
        }
    }

    /// Suma a `frame` el ruido de confort: excitación guardada elegida al azar, filtrada con las
    /// NLSF suavizadas y escalada por la ganancia suavizada menos la de la ocultación.
    fn add_noise(&mut self, input: &CngInput<'_>, frame: &mut [i16]) {
        let gain_q10 = self.noise_gain_q16(input) >> GAIN_Q16_TO_Q10_SHIFT;
        let order = input.lpc_order;
        let length = frame.len();
        let mut sig_q14 = [0i32; MAX_FRAME_LENGTH + MAX_LPC_ORDER];
        self.excitation(&mut sig_q14[MAX_LPC_ORDER..MAX_LPC_ORDER + length]);
        let mut a_q12 = [0i16; MAX_LPC_ORDER];
        nlsf2a(
            &mut a_q12[..order],
            &self.smth_nlsf_q15[..order],
            tables::nlsf_codebook(input.fs_khz).lpc_ordering,
        );
        sig_q14[..MAX_LPC_ORDER].copy_from_slice(&self.synth_state);
        for (i, sample) in frame.iter_mut().enumerate() {
            let mut lpc_pred_q10 = (order as i32) >> 1;
            for (j, &a) in a_q12[..order].iter().enumerate() {
                lpc_pred_q10 = smlawb(
                    lpc_pred_q10,
                    sig_q14[MAX_LPC_ORDER + i - j - 1],
                    i32::from(a),
                );
            }
            let current = add_sat32(
                sig_q14[MAX_LPC_ORDER + i],
                lshift_sat32(lpc_pred_q10, Q10_TO_Q14_SHIFT),
            );
            sig_q14[MAX_LPC_ORDER + i] = current;
            let noise = sat16(rshift_round(smulww(current, gain_q10), OUTPUT_Q8_SHIFT));
            *sample = sat16(i32::from(*sample) + i32::from(noise));
        }
        self.synth_state
            .copy_from_slice(&sig_q14[length..length + MAX_LPC_ORDER]);
    }

    /// Ganancia del ruido, en Q16: la raíz de la energía suavizada menos 32 veces la de la
    /// ocultación, con los 16 bits altos si alguna es grande (`silk_SMULTT`).
    fn noise_gain_q16(&self, input: &CngInput<'_>) -> i32 {
        let plc_gain_q16 = smulww(input.plc_rand_scale_q14, input.plc_prev_gain_q16);
        let smth = self.smth_gain_q16;
        if plc_gain_q16 >= LARGE_PLC_GAIN_Q16 || smth > LARGE_SMTH_GAIN_Q16 {
            let plc_energy = smultt(plc_gain_q16, plc_gain_q16);
            let energy = smultt(smth, smth).wrapping_sub(plc_energy.wrapping_shl(PLC_ENERGY_SHIFT));
            sqrt_approx(energy).wrapping_shl(16)
        } else {
            let plc_energy = smulww(plc_gain_q16, plc_gain_q16);
            let energy = smulww(smth, smth).wrapping_sub(plc_energy.wrapping_shl(PLC_ENERGY_SHIFT));
            sqrt_approx(energy).wrapping_shl(8)
        }
    }

    /// Escribe en `exc_q14` muestras de la excitación guardada elegidas al azar
    /// (`silk_CNG_exc`); la máscara se reduce a la mayor potencia de dos menos uno que no pase de
    /// la longitud.
    fn excitation(&mut self, exc_q14: &mut [i32]) {
        let mut mask = CNG_BUF_MASK_MAX;
        while mask > exc_q14.len() {
            mask >>= 1;
        }
        let mut seed = self.rand_seed;
        for exc in exc_q14.iter_mut() {
            seed = silk_rand(seed);
            *exc = self.exc_buf_q14[(seed >> RAND_INDEX_SHIFT) as usize & mask];
        }
        self.rand_seed = seed;
    }
}

/// Producto de las mitades altas de 16 bits con signo (`silk_SMULTT`).
fn smultt(a32: i32, b32: i32) -> i32 {
    (a32 >> 16) * (b32 >> 16)
}

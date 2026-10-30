//! Decodificador CELT, para los paquetes solo CELT y la capa alta de los híbridos (RFC 6716
//! §4.3).
//!
//! Sigue de cerca la estructura de `celt_decoder.c` de libopus para conservar la conformidad.

use std::ops::Range;

use crate::Error;
use crate::entropy::{BITRES, EcDec};
use crate::packet::{BITS_PER_BYTE, CELT_FRAME_SAMPLES_48K, OPUS_RATE_HZ, celt_end_band};
use crate::pcm::saturate_to_i16;
use synthesis::{MdctBank, SynthesisShape};

mod bands;
mod cwrs;
mod kiss_fft;
mod laplace;
mod lpc;
mod mdct;
mod modes;
mod pitch;
mod plc;
mod quant_bands;
mod rate;
mod static_tables;
mod synthesis;
mod vq;
mod x86_kernels;

const NBANDS_48K_20MS: usize = 21;
const LOG_ENERGY_FLOOR_DB: f32 = -28.0;
/// Máximo de bits de energía fina por banda (`MAX_FINE_BITS` de libopus).
const MAX_FINE_BITS: i32 = 8;
const COMBFILTER_MINPERIOD: usize = 15;
const DECODE_BUFFER_SIZE: usize = 2048;
const COMB_FILTER_GAINS: [[f32; 3]; 3] = [
    [0.306_640_62, 0.217_041_02, 0.129_638_67],
    [0.463_867_2, 0.268_066_4, 0.0],
    [0.799_804_7, 0.100_097_66, 0.0],
];
const TRIM_ICDF: [u8; 11] = [126, 124, 119, 109, 87, 41, 19, 9, 4, 2, 0];
const SPREAD_ICDF: [u8; 4] = [25, 23, 2, 0];
const TAPSET_ICDF: [u8; 3] = [2, 1, 0];
/// LM máximo: tramas de 20 ms.
const MAX_LM: usize = 3;
/// Probabilidad 1/2^15 de la señal de silencio (RFC 6716 §4.3, Tabla 56; `ec_dec_bit_logp(dec, 15)`
/// en `celt_decode_with_ec`).
const SILENCE_LOGP: u32 = 15;
/// Bits que han de quedar para leer el posfiltro de tono (`tell+16 <= total_bits`).
const POSTFILTER_MIN_BITS: i32 = 16;
/// Campos del posfiltro (RFC 6716 §4.3.7.1): octava uniforme en `0..6`, periodo
/// `(16 << octava) + 4 + octava bits - 1`, ganancia de 3 bits en pasos de 3/32.
const POSTFILTER_OCTAVES: u32 = 6;
const POSTFILTER_PITCH_BASE: i32 = 16;
const POSTFILTER_PITCH_EXTRA_BITS: i32 = 4;
const POSTFILTER_GAIN_BITS: u32 = 3;
const POSTFILTER_GAIN_STEP: f32 = 0.093_75;
/// Probabilidad 1/2^3 de las señales de transitorio y de energía intra, y bits que han de quedar
/// para leerlas (RFC 6716 §4.3, Tabla 56).
const TRANSIENT_LOGP: u32 = 3;
const INTRA_LOGP: u32 = 3;
/// Bits que han de quedar para leer la decisión de dispersión, y precisión de su ICDF
/// (`ec_dec_icdf(dec, spread_icdf, 5)`).
const SPREAD_MIN_BITS: i32 = 4;
const SPREAD_ICDF_FTB: u32 = 5;
/// Probabilidad inicial y mínima de la primera señal de refuerzo dinámico de una banda, y cuanto
/// mínimo del refuerzo en bits enteros (`dynalloc_logp = 6`, `IMAX(2, ...)` e
/// `IMAX(6<<BITRES, width)` en `celt_decode_with_ec`).
const DYNALLOC_INITIAL_LOGP: i32 = 6;
const DYNALLOC_MIN_LOGP: i32 = 2;
const DYNALLOC_MIN_QUANTA_BITS: i32 = 6;
/// Bits enteros que han de quedar para leer el recorte de asignación, precisión de su ICDF y valor
/// por defecto si no caben, que es también el recorte neutro de `clt_compute_allocation` (`alloc_trim = ... ec_dec_icdf(dec, trim_icdf, 7) : 5`).
const ALLOC_TRIM_MIN_BITS: i32 = 6;
const TRIM_ICDF_FTB: u32 = 7;
const ALLOC_TRIM_DEFAULT: i32 = 5;
/// LM mínimo y margen sobre LM, en bits enteros, para reservar el bit de anticolapso
/// (`LM>=2 && bits>=((LM+2)<<BITRES)`).
const ANTI_COLLAPSE_MIN_LM: usize = 2;
const ANTI_COLLAPSE_LM_MARGIN: i32 = 2;
/// Crecimiento máximo de la energía de fondo: 0,001 por bloque corto de la trama y de las
/// pérdidas anteriores, hasta 160 bloques (`IMIN(160, st->loss_duration+M)*GCONST(0.001f)`).
const BACKGROUND_MAX_INCREASE_BLOCKS: usize = 160;
const BACKGROUND_INCREASE_PER_BLOCK: f32 = 0.001;
/// Probabilidades de los indicadores TF (`tf_decode`): la primera banda usa 1/2^2 en transitorio y
/// 1/2^4 si no; las siguientes, 1/2^4 y 1/2^5.
const TF_FIRST_LOGP_TRANSIENT: i32 = 2;
const TF_FIRST_LOGP: i32 = 4;
const TF_LOGP_TRANSIENT: i32 = 4;
const TF_LOGP: i32 = 5;
/// Columnas de `TF_SELECT_TABLE` por cada valor de la señal de transitorio
/// (`tf_select_table[LM][4*isTransient+2*tf_select+tf_res[i]]`).
const TF_SELECT_TRANSIENT_STRIDE: usize = 4;
const TF_SELECT_TABLE: [[i8; 8]; 4] = [
    [0, -1, 0, -1, 0, -1, 0, -1],
    [0, -1, 0, -2, 1, 0, 1, -1],
    [0, -2, 0, -3, 2, 0, 1, -1],
    [0, -2, 0, -3, 3, 0, 1, -1],
];

/// Resultado de decodificar una trama CELT.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CeltFrameDecode {
    /// Muestras decodificadas por canal de salida.
    pub samples_per_channel: usize,
}

/// Cómo escribe una trama CELT su salida: sola o sumada a la de SILK, en una trama híbrida.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CeltOutput {
    /// Sustituye lo que hubiera en el búfer de salida.
    Replace,
    /// Se suma, con saturación, a la salida de SILK que ya está en el búfer.
    AddToSilk,
}

/// Decodificador CELT con estado: memorias de síntesis y de deénfasis, historial de energías,
/// posfiltro de tono, semilla del ruido y estado de la ocultación de pérdidas. Guarda siempre
/// el estado de dos canales, como libopus.
#[derive(Debug, Clone)]
pub(crate) struct CeltDecoder {
    fs_hz: u32,
    channels: u8,
    start_band: usize,
    mode: &'static modes::CeltMode,
    mdct_bank: MdctBank,
    decode_mem: Vec<f32>,
    decode_mem_right: Vec<f32>,
    prev_energy: Vec<f32>,
    old_log_energy: Vec<f32>,
    old_log_energy2: Vec<f32>,
    background_log_energy: Vec<f32>,
    deemph_mem: Vec<f32>,
    end_band_override: Option<usize>,
    postfilter_period: i32,
    postfilter_period_old: i32,
    postfilter_gain: f32,
    postfilter_gain_old: f32,
    postfilter_tapset: usize,
    postfilter_tapset_old: usize,
    final_range: u32,
    rng_seed: u32,
    last_split_count: usize,
    /// Bloques cortos perdidos desde la última trama buena (`loss_duration`).
    loss_duration: u32,
    /// Banda siguiente a la última de la última trama buena, con la que se oculta la siguiente
    /// pérdida (`st->end`).
    end_band: usize,
    /// Bloques cortos ocultados seguidos (`plc_duration`); desde 40 se oculta con ruido.
    plc_duration: u32,
    /// Ocultar con ruido aunque se pudiera repetir el tono: hasta que lleguen dos tramas buenas
    /// seguidas tras un reinicio o una ocultación por ruido (`skip_plc`).
    skip_plc: bool,
    /// Si la última trama se ocultó repitiendo el tono; la siguiente pérdida reutiliza su periodo
    /// y su predicción (`last_frame_type == FRAME_PLC_PERIODIC`).
    last_frame_periodic: bool,
    /// Periodo de tono de la última ocultación periódica (`last_pitch_index`).
    last_pitch_index: usize,
    /// La trama siguiente ha de fundir el solapamiento de la ocultación periódica, que no pasó
    /// por el postfiltro (`prefilter_and_fold`).
    prefilter_and_fold: bool,
    /// Predicción lineal de cada canal de la ocultación periódica.
    lpc: [[f32; plc::LPC_ORDER]; 2],
}

impl CeltDecoder {
    /// Crea el decodificador CELT para la frecuencia de salida `fs_hz` y `channels` canales de
    /// salida, sin estado previo.
    pub fn new(fs_hz: u32, channels: u8) -> Self {
        let mode = modes::mode48000_960_120();
        let state_ch = 2usize;
        Self {
            fs_hz,
            channels,
            start_band: 0,
            mode,
            mdct_bank: MdctBank::new(mode),
            decode_mem: vec![0.0; DECODE_BUFFER_SIZE + mode.overlap],
            decode_mem_right: vec![0.0; DECODE_BUFFER_SIZE + mode.overlap],
            prev_energy: vec![0.0; NBANDS_48K_20MS * state_ch],
            old_log_energy: vec![LOG_ENERGY_FLOOR_DB; NBANDS_48K_20MS * state_ch],
            old_log_energy2: vec![LOG_ENERGY_FLOOR_DB; NBANDS_48K_20MS * state_ch],
            background_log_energy: vec![0.0; NBANDS_48K_20MS * state_ch],
            deemph_mem: vec![0.0; state_ch],
            end_band_override: None,
            postfilter_period: 0,
            postfilter_period_old: 0,
            postfilter_gain: 0.0,
            postfilter_gain_old: 0.0,
            postfilter_tapset: 0,
            postfilter_tapset_old: 0,
            final_range: 0,
            rng_seed: 0,
            last_split_count: 0,
            loss_duration: 0,
            end_band: mode.nb_ebands,
            plc_duration: 0,
            skip_plc: true,
            last_frame_periodic: false,
            last_pitch_index: 0,
            prefilter_and_fold: false,
            lpc: [[0.0; plc::LPC_ORDER]; 2],
        }
    }

    /// Vuelve al estado recién creado (entre flujos o al cambiar de modo).
    pub fn reset(&mut self) {
        self.decode_mem.fill(0.0);
        self.decode_mem_right.fill(0.0);
        self.prev_energy.fill(0.0);
        self.old_log_energy.fill(LOG_ENERGY_FLOOR_DB);
        self.old_log_energy2.fill(LOG_ENERGY_FLOOR_DB);
        self.background_log_energy.fill(0.0);
        self.deemph_mem.fill(0.0);
        self.end_band_override = None;
        self.reset_postfilter();
        self.final_range = 0;
        self.rng_seed = 0;
        self.last_split_count = 0;
        self.start_band = 0;
        self.loss_duration = 0;
        self.plc_duration = 0;
        self.skip_plc = true;
        self.last_frame_periodic = false;
        self.last_pitch_index = 0;
        self.prefilter_and_fold = false;
        self.lpc = [[0.0; plc::LPC_ORDER]; 2];
    }

    /// Anula el postfiltro de tono: periodo, ganancia y juego de coeficientes, actuales y
    /// anteriores.
    fn reset_postfilter(&mut self) {
        self.postfilter_period = 0;
        self.postfilter_period_old = 0;
        self.postfilter_gain = 0.0;
        self.postfilter_gain_old = 0.0;
        self.postfilter_tapset = 0;
        self.postfilter_tapset_old = 0;
    }

    /// Bandas, canales codificados y duración de la trama para decodificar su energía.
    fn energy_frame(&self, layout: &FrameLayout) -> quant_bands::EnergyFrame<'static> {
        quant_bands::EnergyFrame {
            mode: self.mode,
            start: layout.start,
            end: layout.end,
            channels: layout.coded_channels,
            lm: layout.lm,
        }
    }

    /// Memoria de síntesis del canal `ch` (0 izquierdo, 1 derecho).
    fn channel_mem(&self, ch: usize) -> &[f32] {
        if ch == 0 {
            &self.decode_mem
        } else {
            &self.decode_mem_right
        }
    }

    /// Factor de diezmado de 48 kHz a la frecuencia de salida.
    ///
    /// # Errores
    ///
    /// `NotImplemented` si la frecuencia de salida no está admitida.
    fn downsample_factor(&self) -> Result<usize, Error> {
        if !crate::pcm::SUPPORTED_RATES_HZ.contains(&self.fs_hz) {
            return Err(Error::NotImplemented);
        }
        Ok(OPUS_RATE_HZ / self.fs_hz as usize)
    }

    /// Muestras por canal a la frecuencia de salida de una trama de `frame_size_48k` muestras a
    /// 48 kHz.
    ///
    /// # Errores
    ///
    /// `NotImplemented` si la frecuencia no está admitida o no divide la trama.
    fn output_frame_size(&self, frame_size_48k: usize) -> Result<usize, Error> {
        let downsample = self.downsample_factor()?;
        if frame_size_48k % downsample != 0 {
            return Err(Error::NotImplemented);
        }
        Ok(frame_size_48k / downsample)
    }

    /// Escribe el canal `ch` ya sintetizado y con deénfasis, `ch_synth`, diezmado a la frecuencia
    /// de salida, en el búfer entrelazado `out`, según `output`.
    ///
    /// # Errores
    ///
    /// Los de [`Self::output_frame_size`].
    fn write_output_channel_i16(
        &self,
        out: &mut [i16],
        ch_synth: &[f32],
        ch: usize,
        output: CeltOutput,
    ) -> Result<(), Error> {
        let downsample = self.downsample_factor()?;
        let output_samples = self.output_frame_size(ch_synth.len())?;
        for i in 0..output_samples {
            let sample = saturate_to_i16(ch_synth[i * downsample]);
            let out_idx = i * self.channels as usize + ch;
            out[out_idx] = if output == CeltOutput::AddToSilk {
                out[out_idx].saturating_add(sample)
            } else {
                sample
            };
        }
        Ok(())
    }

    /// Oculta una trama CELT perdida de `frame_size` muestras por canal y escribe su salida en
    /// `out` según `output`, como `celt_decode_lost` de libopus con `FRAME_PLC_NOISE`: ruido con
    /// la energía de la última trama, que cae sin bajar de la de fondo, en las bandas desde la
    /// inicial, y el postfiltro con los últimos parámetros. libopus repite en su lugar el periodo
    /// de tono de la señal anterior (`FRAME_PLC_PERIODIC`) cuando la trama empieza en la banda 0
    /// y las pérdidas son recientes; aquí aún no (defecto conocido).
    ///
    /// # Errores
    ///
    /// `NotImplemented` si la duración no es de CELT o la frecuencia de salida no está admitida,
    /// y `OutputTooSmall` si la trama no cabe en `out`.
    pub fn decode_lost(
        &mut self,
        frame_size: usize,
        out: &mut [i16],
        output: CeltOutput,
    ) -> Result<usize, Error> {
        let downsample = self.downsample_factor()?;
        let frame_size_48k = frame_size
            .checked_mul(downsample)
            .ok_or(Error::NotImplemented)?;
        let lm = lm_for_frame_size(frame_size_48k).ok_or(Error::NotImplemented)?;
        let channels = self.channels as usize;
        let needed = frame_size * channels;
        if out.len() < needed {
            return Err(Error::OutputTooSmall {
                needed,
                got: out.len(),
            });
        }

        let periodic = self.start_band == 0
            && !self.skip_plc
            && self.plc_duration < plc::MAX_PERIODIC_PLC_DURATION;
        if periodic {
            self.conceal_periodic(frame_size_48k);
        } else {
            self.conceal_with_noise(frame_size_48k, lm, downsample)?;
        }
        let out_start = Self::out_start(frame_size_48k);
        for ch in 0..channels {
            let mut ch_synth = self.channel_mem(ch)[out_start..out_start + frame_size_48k].to_vec();
            self.apply_deemph(ch, &mut ch_synth);
            self.write_output_channel_i16(out, &ch_synth, ch, output)?;
        }

        self.final_range = 0;
        self.last_split_count = 0;
        self.loss_duration = (self.loss_duration + (1 << lm)).min(plc::LOSS_DURATION_MAX);
        self.plc_duration = (self.plc_duration + (1 << lm)).min(plc::LOSS_DURATION_MAX);
        self.last_frame_periodic = periodic;
        Ok(frame_size)
    }

    /// Oculta la trama con ruido (`FRAME_PLC_NOISE`): la energía de la última trama cae sin bajar
    /// de la de fondo, el ruido llena las bandas desde la inicial y se filtra con los últimos
    /// parámetros del postfiltro. Después de una ocultación por ruido no se repite el tono hasta
    /// que lleguen dos tramas buenas seguidas.
    ///
    /// # Errores
    ///
    /// Los de la MDCT inversa.
    fn conceal_with_noise(
        &mut self,
        frame_size_48k: usize,
        lm: usize,
        downsample: usize,
    ) -> Result<(), Error> {
        let channels = self.channels as usize;
        if self.prefilter_and_fold {
            self.prefilter_and_fold();
        }
        let nb_ebands = self.mode.nb_ebands;
        let start = self.start_band;
        let end = self.end_band;
        // `effEnd`: en el modo de 48 kHz todas las bandas son efectivas (`effEBands` = 21).
        let eff_end = start.max(end.min(nb_ebands));
        let decay = plc::noise_decay(self.loss_duration);
        let shape = SynthesisShape {
            frame_size_48k,
            lm,
            is_transient: false,
        };
        for ch in 0..channels {
            let energy = ch * nb_ebands..(ch + 1) * nb_ebands;
            plc::decay_energy(
                &mut self.prev_energy[energy.clone()],
                &self.background_log_energy[energy],
                start..end,
                decay,
            );
        }
        for ch in 0..channels {
            let mut spectrum = vec![0.0f32; frame_size_48k];
            self.rng_seed = plc::fill_noise(
                &mut spectrum,
                self.mode.e_bands,
                start..eff_end,
                lm,
                self.rng_seed,
            );
            let mut denorm = vec![0.0f32; frame_size_48k];
            bands::denormalise_bands(
                self.mode,
                &spectrum,
                &mut denorm,
                &self.prev_energy[ch * nb_ebands..(ch + 1) * nb_ebands],
                start..eff_end,
                lm,
                downsample,
            );
            let mem = if ch == 0 {
                &mut self.decode_mem
            } else {
                &mut self.decode_mem_right
            };
            self.mdct_bank.synthesise_into(&denorm, shape, mem)?;
        }

        let current = self.current_postfilter_taps();
        self.run_postfilter(lm, frame_size_48k, Self::out_start(frame_size_48k), current);
        self.prefilter_and_fold = false;
        self.skip_plc = true;
        Ok(())
    }

    /// Oculta la trama repitiendo el periodo de tono de la señal anterior en cada canal
    /// (`FRAME_PLC_PERIODIC`). La primera pérdida busca el periodo y calcula la predicción; las
    /// siguientes los reutilizan con una atenuación adicional. La trama siguiente funde el
    /// solapamiento.
    fn conceal_periodic(&mut self, frame_size_48k: usize) {
        let first = !self.last_frame_periodic;
        let (pitch, fade) = if first {
            let channels: Vec<&[f32]> = (0..self.channels as usize)
                .map(|ch| self.channel_mem(ch))
                .collect();
            let pitch = pitch::plc_pitch_search(&channels, DECODE_BUFFER_SIZE >> 1);
            self.last_pitch_index = pitch;
            (pitch, 1.0)
        } else {
            (self.last_pitch_index, plc::REPEATED_LOSS_FADE)
        };
        let params = plc::PeriodicConcealment {
            window: self.mode.window,
            pitch,
            fade,
            frame_size_48k,
            recompute_lpc: first,
        };
        let [lpc_left, lpc_right] = &mut self.lpc;
        plc::conceal_periodic(&mut self.decode_mem, lpc_left, &params);
        if self.channels == 2 {
            plc::conceal_periodic(&mut self.decode_mem_right, lpc_right, &params);
        }
        self.prefilter_and_fold = true;
    }

    /// Funde con la trama que se va a sintetizar el solapamiento de una ocultación periódica, que
    /// no pasó por el postfiltro (`prefilter_and_fold`): le aplica el prefiltro (el postfiltro con
    /// las ganancias cambiadas de signo) y simula la cancelación del solapamiento de la MDCT.
    /// libopus lo hace tras desplazar la memoria una trama, en `DECODE_BUFFER_SIZE - N`; aquí, antes
    /// del desplazamiento, en `DECODE_BUFFER_SIZE`, que es la misma muestra.
    fn prefilter_and_fold(&mut self) {
        let overlap = self.mode.overlap;
        let window = self.mode.window;
        let gain = -self.postfilter_gain;
        let period = self.postfilter_period.max(COMBFILTER_MINPERIOD as i32) as usize;
        let taps = COMB_FILTER_GAINS[self.postfilter_tapset.min(2)];
        let channels = self.channels as usize;
        for mem in [&mut self.decode_mem, &mut self.decode_mem_right]
            .into_iter()
            .take(channels)
        {
            // Con la ganancia nueva a cero, `comb_filter` copia la entrada.
            let etmp = if gain == 0.0 {
                mem[DECODE_BUFFER_SIZE..DECODE_BUFFER_SIZE + overlap].to_vec()
            } else {
                x86_kernels::comb_filter_const_sse(
                    mem,
                    DECODE_BUFFER_SIZE,
                    period,
                    overlap,
                    [gain * taps[0], gain * taps[1], gain * taps[2]],
                )
            };
            for i in 0..overlap / 2 {
                mem[DECODE_BUFFER_SIZE + i] =
                    window[i] * etmp[overlap - 1 - i] + window[overlap - 1 - i] * etmp[i];
            }
        }
    }

    /// Estado final del decodificador de rango tras la última trama.
    pub fn final_range(&self) -> u32 {
        self.final_range
    }

    /// Ventana de solapamiento de la MDCT, que también usan los fundidos de las transiciones.
    pub(crate) fn window(&self) -> &[f32] {
        self.mode.window
    }

    /// Memoria del filtro de deénfasis del canal `ch`.
    pub fn deemph_mem(&self, ch: usize) -> f32 {
        self.deemph_mem[ch]
    }

    /// Particiones recursivas de la última trama (para depurar).
    pub fn last_split_count(&self) -> usize {
        self.last_split_count
    }

    /// Primera banda que decodifican las tramas siguientes (17 en híbrido), acotada a las del
    /// modo.
    pub fn set_start_band(&mut self, band: usize) {
        self.start_band = band.min(self.mode.nb_ebands.saturating_sub(1));
    }

    /// Fija la banda siguiente a la última que se decodifica, acotada a `1..=nb_ebands`, en lugar
    /// de la que da la configuración del TOC.
    pub fn set_end_band(&mut self, band: usize) {
        self.end_band_override = Some(band.clamp(1, self.mode.nb_ebands));
    }

    /// Vuelve a tomar de la configuración del TOC la última banda que se decodifica.
    pub fn clear_end_band(&mut self) {
        self.end_band_override = None;
    }

    /// Filtro de deénfasis de primer orden sobre las muestras del canal `ch`, con su memoria.
    fn apply_deemph(&mut self, ch: usize, samples: &mut [f32]) {
        let coef = self.mode.preemph[0];
        let mut mem = self.deemph_mem[ch];
        for x in samples.iter_mut() {
            // Recurrencia de libopus: tmp = x + m; m = coef * tmp; salida = tmp.
            let tmp = *x + mem;
            mem = coef * tmp;
            *x = tmp;
        }
        self.deemph_mem[ch] = mem;
    }

    /// Aplica en `buf[start..start + n]` el filtro peine de tono (`comb_filter`), pasando de los
    /// parámetros `from` a `to` a lo largo de los primeros `overlap` coeficientes con la ventana
    /// `window`.
    fn comb_filter_in_place(
        buf: &mut [f32],
        start: usize,
        n: usize,
        from: PitchTaps,
        to: PitchTaps,
        window: &[f32],
        overlap: usize,
    ) {
        let PitchTaps {
            period: t0,
            gain: g0,
            tapset: tapset0,
        } = from;
        let PitchTaps {
            period: t1,
            gain: g1,
            tapset: tapset1,
        } = to;
        if n == 0 || (g0 == 0.0 && g1 == 0.0) {
            return;
        }
        let period0 = t0.max(COMBFILTER_MINPERIOD);
        let period1 = t1.max(COMBFILTER_MINPERIOD);
        let ts0 = tapset0.min(2);
        let ts1 = tapset1.min(2);
        let g00 = g0 * COMB_FILTER_GAINS[ts0][0];
        let g01 = g0 * COMB_FILTER_GAINS[ts0][1];
        let g02 = g0 * COMB_FILTER_GAINS[ts0][2];
        let g10 = g1 * COMB_FILTER_GAINS[ts1][0];
        let g11 = g1 * COMB_FILTER_GAINS[ts1][1];
        let g12 = g1 * COMB_FILTER_GAINS[ts1][2];
        let mut overlap_len = overlap.min(n);
        if g0 == g1 && period0 == period1 && ts0 == ts1 {
            overlap_len = 0;
        }

        let mut x1 = buf[start - period1 + 1];
        let mut x2 = buf[start - period1];
        let mut x3 = buf[start - period1 - 1];
        let mut x4 = buf[start - period1 - 2];

        for i in 0..overlap_len {
            let x0 = buf[start + i - period1 + 2];
            let f = window[i] * window[i];
            let one_minus_f = 1.0 - f;
            let mut y = buf[start + i];
            y += (one_minus_f * g00) * buf[start + i - period0];
            y +=
                (one_minus_f * g01) * (buf[start + i - period0 + 1] + buf[start + i - period0 - 1]);
            y +=
                (one_minus_f * g02) * (buf[start + i - period0 + 2] + buf[start + i - period0 - 2]);
            y += (f * g10) * x2;
            y += (f * g11) * (x1 + x3);
            y += (f * g12) * (x0 + x4);
            buf[start + i] = y;
            x4 = x3;
            x3 = x2;
            x2 = x1;
            x1 = x0;
        }

        if g1 == 0.0 {
            return;
        }
        // Fuera del solapamiento, libopus x86 filtra con `comb_filter_const_sse`, que suma en
        // otro orden que su versión en C.
        for i in overlap_len..n {
            let x0 = buf[start + i - period1 + 2];
            buf[start + i] = x86_kernels::comb_filter_const_sample(
                buf[start + i],
                x2,
                x3 + x1,
                x4 + x0,
                [g10, g11, g12],
            );
            x4 = x3;
            x3 = x2;
            x2 = x1;
            x1 = x0;
        }
    }

    /// Posición de `decode_mem` donde empieza la salida de una trama de `frame_size_48k`
    /// muestras a 48 kHz.
    fn out_start(frame_size_48k: usize) -> usize {
        DECODE_BUFFER_SIZE - frame_size_48k
    }

    /// Decodifica una trama CELT sola (la redundancia de las transiciones) de `frame_size_48k`
    /// muestras por canal a 48 kHz (120, 240, 480 o 960).
    ///
    /// # Errores
    ///
    /// Los de [`Self::decode_frame_with_ec`].
    pub fn decode_frame(
        &mut self,
        frame: &[u8],
        frame_size_48k: usize,
        config: u8,
        packet_channels: u8,
        out: &mut [i16],
    ) -> Result<CeltFrameDecode, Error> {
        let mut ec = EcDec::new(frame);
        self.decode_frame_with_ec(
            &mut ec,
            frame_size_48k,
            config,
            packet_channels,
            out,
            CeltOutput::Replace,
        )
    }

    /// Decodifica una trama CELT con el decodificador de rango `ec`, que en una trama híbrida
    /// comparte con SILK, y escribe su salida en `out` según `output`.
    ///
    /// # Errores
    ///
    /// `NotImplemented` si la duración no es de CELT, los canales de salida no son 1 ni 2 o la
    /// frecuencia de salida no está admitida, y `OutputTooSmall` si la trama no cabe en `out`.
    pub(crate) fn decode_frame_with_ec(
        &mut self,
        ec: &mut EcDec<'_>,
        frame_size_48k: usize,
        config: u8,
        packet_channels: u8,
        out: &mut [i16],
        output: CeltOutput,
    ) -> Result<CeltFrameDecode, Error> {
        let output_samples = self.output_frame_size(frame_size_48k)?;
        if !matches!(self.channels, 1 | 2) {
            return Err(Error::NotImplemented);
        }
        let lm = lm_for_frame_size(frame_size_48k).ok_or(Error::NotImplemented)?;
        let needed = output_samples * self.channels as usize;
        if out.len() < needed {
            return Err(Error::OutputTooSmall {
                needed,
                got: out.len(),
            });
        }

        // La ocultación periódica solo vuelve tras dos tramas buenas seguidas.
        if self.loss_duration == 0 {
            self.skip_plc = false;
        }
        let coded_channels = packet_channels.clamp(1, 2) as usize;
        if coded_channels == 1 {
            let nb = self.mode.nb_ebands;
            for i in 0..nb {
                self.prev_energy[i] = self.prev_energy[i].max(self.prev_energy[nb + i]);
            }
        }
        let start = self.start_band.min(self.mode.nb_ebands.saturating_sub(1));
        let end = self
            .end_band_override
            .unwrap_or_else(|| celt_end_band(config));
        let active_len = ec.storage();
        let total_bits = (active_len * BITS_PER_BYTE) as i32;
        let layout = FrameLayout {
            start,
            end,
            lm,
            coded_channels,
            frame_size_48k,
            downsample: self.downsample_factor()?,
            total_bits,
        };
        let header = decode_frame_header(ec, &layout);
        let allocation = self.decode_allocation(ec, &layout, &header);
        let shapes = self.decode_band_shapes(ec, &layout, &header, &allocation);
        let shapes = self.finalise_energy(ec, &layout, &header, &allocation, shapes);
        let (denorm, denorm_right_for_stereo) =
            self.denormalise_channels(&layout, &header, &shapes);
        if self.prefilter_and_fold {
            self.prefilter_and_fold();
        }
        let out_start = self.synthesise(&layout, &header, denorm, denorm_right_for_stereo)?;
        self.apply_postfilter(&layout, &header, out_start);
        self.update_energy_history(&layout, &header);
        for ch in 0..self.channels as usize {
            let mut ch_synth = self.channel_mem(ch)[out_start..out_start + frame_size_48k].to_vec();
            self.apply_deemph(ch, &mut ch_synth);
            self.write_output_channel_i16(out, &ch_synth, ch, output)?;
        }

        self.final_range = ec.final_range();
        self.rng_seed = ec.rng();
        self.loss_duration = 0;
        self.plc_duration = 0;
        self.last_frame_periodic = false;
        self.prefilter_and_fold = false;
        self.end_band = end;
        Ok(CeltFrameDecode {
            samples_per_channel: output_samples,
        })
    }

    /// Energía gruesa, resolución tiempo-frecuencia, dispersión, asignación dinámica, recorte,
    /// reserva de anticolapso, reparto de bits y energía fina.
    fn decode_allocation(
        &mut self,
        ec: &mut EcDec<'_>,
        layout: &FrameLayout,
        header: &FrameHeader,
    ) -> FrameAllocation {
        let FrameLayout {
            start,
            end,
            lm,
            coded_channels,
            total_bits,
            ..
        } = *layout;
        let intra_ener = header.intra_ener;
        let is_transient = header.is_transient;
        if !intra_ener && self.loss_duration != 0 {
            self.make_energy_prediction_safe(start..end, lm);
        }
        quant_bands::unquant_coarse_energy(
            &self.energy_frame(layout),
            &mut self.prev_energy,
            intra_ener,
            ec,
            total_bits,
        );
        let mut tf_res = vec![0i32; self.mode.nb_ebands];
        tf_decode(start, end, is_transient, &mut tf_res, lm, total_bits, ec);
        let spread_decision = if ec.tell() + SPREAD_MIN_BITS <= total_bits {
            ec.dec_icdf(&SPREAD_ICDF, SPREAD_ICDF_FTB)
        } else {
            0
        };
        let cap = rate::init_caps(self.mode, lm, coded_channels);
        let (offsets, tell, total_bits_q) = self.decode_dynalloc(ec, layout, &cap);
        let alloc_trim = if tell + (ALLOC_TRIM_MIN_BITS << BITRES) <= total_bits_q {
            ec.dec_icdf(&TRIM_ICDF, TRIM_ICDF_FTB)
        } else {
            ALLOC_TRIM_DEFAULT
        };
        let mut bits = (total_bits << BITRES) - ec.tell_frac() as i32 - 1;
        let anti_collapse_rsv = if is_transient
            && lm >= ANTI_COLLAPSE_MIN_LM
            && bits >= ((lm as i32 + ANTI_COLLAPSE_LM_MARGIN) << BITRES)
        {
            1 << BITRES
        } else {
            0
        };
        bits -= anti_collapse_rsv;
        let alloc = rate::clt_compute_allocation(
            &rate::AllocationRequest {
                mode: self.mode,
                start,
                end,
                offsets: &offsets,
                cap: &cap,
                alloc_trim,
                total: bits,
                channels: coded_channels,
                lm,
            },
            ec,
        );
        quant_bands::unquant_fine_energy(
            &self.energy_frame(layout),
            &mut self.prev_energy,
            &alloc.fine_quant,
            ec,
        );
        FrameAllocation {
            tf_res,
            spread_decision,
            alloc,
            anti_collapse_rsv,
        }
    }

    /// Tras una pérdida, hace segura la predicción de energía de las bandas `bands` de los dos
    /// canales del estado antes de leer la energía gruesa, como `celt_decode_with_ec` de libopus.
    fn make_energy_prediction_safe(&mut self, bands: Range<usize>, lm: usize) {
        let nb_ebands = self.mode.nb_ebands;
        for ch in 0..2 {
            let channel = ch * nb_ebands..(ch + 1) * nb_ebands;
            plc::make_energy_prediction_safe(
                plc::EnergyHistory {
                    last: &mut self.prev_energy[channel.clone()],
                    previous: &self.old_log_energy[channel.clone()],
                    before_previous: &self.old_log_energy2[channel],
                },
                bands.clone(),
                lm,
                self.loss_duration,
            );
        }
    }

    /// Asignación dinámica de bits por banda (`dynalloc` en `celt_decode_with_ec`). Devuelve el
    /// impulso de cada banda, la posición en el flujo en octavos de bit y los bits que quedan,
    /// también en octavos.
    fn decode_dynalloc(
        &self,
        ec: &mut EcDec<'_>,
        layout: &FrameLayout,
        cap: &[i32],
    ) -> (Vec<i32>, i32, i32) {
        let FrameLayout {
            start,
            end,
            lm,
            coded_channels,
            total_bits,
            ..
        } = *layout;
        let mut offsets = vec![0i32; self.mode.nb_ebands];
        let mut dynalloc_logp = DYNALLOC_INITIAL_LOGP;
        let mut total_bits_q = total_bits << BITRES;
        let mut tell = ec.tell_frac() as i32;
        for i in start..end {
            let width = (coded_channels as i32)
                * (((self.mode.e_bands[i + 1] - self.mode.e_bands[i]) as i32) << lm);
            let quanta =
                ((width << BITRES).min((DYNALLOC_MIN_QUANTA_BITS << BITRES).max(width))).max(0);
            let mut dynalloc_loop_logp = dynalloc_logp;
            let mut boost = 0i32;
            while tell + (dynalloc_loop_logp << BITRES) < total_bits_q && boost < cap[i] {
                let flag = ec.dec_bit_logp(dynalloc_loop_logp as u32);
                tell = ec.tell_frac() as i32;
                if !flag {
                    break;
                }
                boost += quanta;
                total_bits_q -= quanta;
                dynalloc_loop_logp = 1;
            }
            offsets[i] = boost;
            if boost > 0 {
                dynalloc_logp = (dynalloc_logp - 1).max(DYNALLOC_MIN_LOGP);
            }
        }
        (offsets, tell, total_bits_q)
    }

    /// Decodifica la forma (PVQ) de todas las bandas de la trama.
    fn decode_band_shapes(
        &mut self,
        ec: &mut EcDec<'_>,
        layout: &FrameLayout,
        header: &FrameHeader,
        allocation: &FrameAllocation,
    ) -> BandShapes {
        let FrameLayout {
            start,
            end,
            lm,
            coded_channels,
            frame_size_48k,
            total_bits,
            ..
        } = *layout;
        let is_transient = header.is_transient;
        let tf_res = &allocation.tf_res;
        let spread_decision = allocation.spread_decision;
        let alloc = &allocation.alloc;
        let anti_collapse_rsv = allocation.anti_collapse_rsv;
        let mut mdct_in = vec![0.0f32; frame_size_48k];
        let mut mdct_side = if coded_channels == 2 {
            Some(vec![0.0f32; frame_size_48k])
        } else {
            None
        };
        let frame = bands::BandsFrame {
            mode: self.mode,
            start,
            end,
            pulses: &alloc.pulses,
            short_blocks: is_transient,
            spread: spread_decision,
            tf_res,
            total_bits_q: (total_bits << BITRES) - anti_collapse_rsv,
            balance: alloc.balance,
            coded_bands: alloc.coded_bands,
            lm,
        };
        let collapse_masks = if let Some(ref mut side) = mdct_side {
            bands::quant_all_bands_stereo(
                &frame,
                &mut mdct_in,
                side,
                ec,
                &mut self.rng_seed,
                bands::StereoParams {
                    dual_stereo: alloc.dual_stereo != 0,
                    intensity: alloc.intensity,
                    disable_inv: coded_channels == 2 && self.channels == 1,
                },
            )
        } else {
            bands::quant_all_bands_mono(&frame, &mut mdct_in, ec, &mut self.rng_seed)
        };
        BandShapes {
            mdct_in,
            mdct_side,
            collapse_masks,
        }
    }

    /// Bit de anticolapso, bits de energía sobrantes, anticolapso y energía de las tramas de
    /// silencio.
    fn finalise_energy(
        &mut self,
        ec: &mut EcDec<'_>,
        layout: &FrameLayout,
        header: &FrameHeader,
        allocation: &FrameAllocation,
        shapes: BandShapes,
    ) -> BandShapes {
        let BandShapes {
            mut mdct_in,
            mut mdct_side,
            collapse_masks,
        } = shapes;
        let FrameLayout {
            coded_channels,
            total_bits,
            ..
        } = *layout;
        let silence_flag = header.silence;
        let alloc = &allocation.alloc;
        let anti_collapse_rsv = allocation.anti_collapse_rsv;
        self.last_split_count = 0;
        // Bit de anticolapso, solo si se reservó (tramas transitorias).
        let mut anti_collapse_on = 0u32;
        if anti_collapse_rsv > 0 {
            anti_collapse_on = ec.dec_bits(1);
        }
        quant_bands::unquant_energy_finalise(
            &self.energy_frame(layout),
            &mut self.prev_energy,
            &alloc.fine_quant,
            &alloc.fine_priority,
            total_bits - ec.tell(),
            ec,
        );
        if anti_collapse_on != 0 {
            bands::anti_collapse(
                &self.energy_frame(layout),
                &mut mdct_in,
                mdct_side.as_deref_mut(),
                &collapse_masks,
                &bands::EnergyHistory {
                    current: &self.prev_energy,
                    prev1: &self.old_log_energy,
                    prev2: &self.old_log_energy2,
                },
                &alloc.pulses,
                self.rng_seed,
            );
        }
        if silence_flag {
            let state_channels = coded_channels.min(2);
            for c in 0..state_channels {
                let base = c * self.mode.nb_ebands;
                for i in 0..self.mode.nb_ebands {
                    self.prev_energy[base + i] = LOG_ENERGY_FLOOR_DB;
                }
            }
        }
        BandShapes {
            mdct_in,
            mdct_side,
            collapse_masks,
        }
    }

    /// Pasa las bandas normalizadas a espectro con su energía y reparte los canales codificados
    /// entre los de salida. Devuelve el espectro izquierdo (o mono) y, en salida estéreo, el
    /// derecho.
    fn denormalise_channels(
        &self,
        layout: &FrameLayout,
        header: &FrameHeader,
        shapes: &BandShapes,
    ) -> (Vec<f32>, Option<Vec<f32>>) {
        let mdct_in = &shapes.mdct_in;
        let mdct_side = &shapes.mdct_side;
        let FrameLayout {
            start,
            end,
            lm,
            coded_channels,
            frame_size_48k,
            downsample,
            ..
        } = *layout;
        let silence_flag = header.silence;
        let mut denorm = vec![0.0f32; frame_size_48k];
        if !silence_flag {
            bands::denormalise_bands(
                self.mode,
                mdct_in,
                &mut denorm,
                &self.prev_energy[..self.mode.nb_ebands],
                start..end,
                lm,
                downsample,
            );
        }
        let mut denorm_right_for_stereo: Option<Vec<f32>> = None;
        if coded_channels == 2 {
            if let Some(side) = mdct_side.as_ref() {
                let mut denorm_side = vec![0.0f32; frame_size_48k];
                if !silence_flag {
                    bands::denormalise_bands(
                        self.mode,
                        side,
                        &mut denorm_side,
                        &self.prev_energy[self.mode.nb_ebands..self.mode.nb_ebands * 2],
                        start..end,
                        lm,
                        downsample,
                    );
                }
                if self.channels == 1 {
                    for i in 0..frame_size_48k {
                        denorm[i] = 0.5 * (denorm[i] + denorm_side[i]);
                    }
                } else if self.channels == 2 {
                    denorm_right_for_stereo = Some(denorm_side);
                }
            }
        } else if self.channels == 2 {
            // Paquete mono con salida estéreo: el mismo espectro en los dos canales.
            denorm_right_for_stereo = Some(denorm.clone());
        }

        (denorm, denorm_right_for_stereo)
    }

    /// Aplica la MDCT inversa de cada canal de salida sobre su memoria de síntesis; una trama
    /// codificada en mono que sale en estéreo repite el espectro izquierdo en el derecho.
    /// Devuelve dónde empieza la trama nueva en la memoria.
    fn synthesise(
        &mut self,
        layout: &FrameLayout,
        header: &FrameHeader,
        denorm: Vec<f32>,
        denorm_right_for_stereo: Option<Vec<f32>>,
    ) -> Result<usize, Error> {
        let shape = SynthesisShape {
            frame_size_48k: layout.frame_size_48k,
            lm: layout.lm,
            is_transient: header.is_transient,
        };
        self.mdct_bank
            .synthesise_into(&denorm, shape, &mut self.decode_mem)?;
        if self.channels == 2 {
            let right = denorm_right_for_stereo.as_deref().unwrap_or(&denorm);
            self.mdct_bank
                .synthesise_into(right, shape, &mut self.decode_mem_right)?;
        }
        Ok(Self::out_start(layout.frame_size_48k))
    }

    /// Aplica el filtro peine (postfiltro de tono) a cada canal y guarda sus parámetros para la
    /// trama siguiente.
    fn apply_postfilter(&mut self, layout: &FrameLayout, header: &FrameHeader, out_start: usize) {
        let new = PitchTaps {
            period: header.postfilter.pitch.max(0) as usize,
            gain: header.postfilter.gain,
            tapset: header.postfilter.tapset.clamp(0, 2) as usize,
        };
        self.run_postfilter(layout.lm, layout.frame_size_48k, out_start, new);
    }

    /// Parámetros del postfiltro de la última trama, con los que se filtra la trama oculta.
    fn current_postfilter_taps(&self) -> PitchTaps {
        PitchTaps {
            period: self.postfilter_period.max(0) as usize,
            gain: self.postfilter_gain,
            tapset: self.postfilter_tapset,
        }
    }

    /// Aplica el postfiltro a la trama de `frame_size_48k` muestras que empieza en `out_start`:
    /// en el primer bloque corto pasa de los parámetros anteriores a los actuales y, en el resto,
    /// de los actuales a `new`, que pasan a ser los actuales.
    fn run_postfilter(
        &mut self,
        lm: usize,
        frame_size_48k: usize,
        out_start: usize,
        new: PitchTaps,
    ) {
        let pf_period = self.postfilter_period.max(COMBFILTER_MINPERIOD as i32) as usize;
        let pf_period_old = self.postfilter_period_old.max(COMBFILTER_MINPERIOD as i32) as usize;
        let pass = CombFilterPass {
            out_start,
            frame_size_48k,
            lm,
            short_n: self.mode.short_mdct_size.min(frame_size_48k),
            old: PitchTaps {
                period: pf_period_old,
                gain: self.postfilter_gain_old,
                tapset: self.postfilter_tapset_old,
            },
            current: PitchTaps {
                period: pf_period,
                gain: self.postfilter_gain,
                tapset: self.postfilter_tapset,
            },
            new,
            window: self.mode.window,
            overlap: self.mode.overlap,
        };
        let channels = self.channels as usize;
        for mem in [&mut self.decode_mem, &mut self.decode_mem_right]
            .into_iter()
            .take(channels)
        {
            pass.apply(mem);
        }
        self.postfilter_period_old = self.postfilter_period;
        self.postfilter_gain_old = self.postfilter_gain;
        self.postfilter_tapset_old = self.postfilter_tapset;
        self.postfilter_period = new.period as i32;
        self.postfilter_gain = new.gain;
        self.postfilter_tapset = new.tapset;
        if lm != 0 {
            self.postfilter_period_old = self.postfilter_period;
            self.postfilter_gain_old = self.postfilter_gain;
            self.postfilter_tapset_old = self.postfilter_tapset;
        }
    }

    /// Actualiza el historial de energía (anterior, previa a la anterior y de fondo) y limpia las
    /// bandas fuera de `start..end`.
    fn update_energy_history(&mut self, layout: &FrameLayout, header: &FrameHeader) {
        let FrameLayout {
            start,
            end,
            lm,
            coded_channels,
            ..
        } = *layout;
        let is_transient = header.is_transient;
        let nb_ebands = self.mode.nb_ebands;
        let state_len = 2 * nb_ebands;
        if coded_channels == 1 {
            let (left, right) = self.prev_energy.split_at_mut(nb_ebands);
            right.copy_from_slice(left);
        }
        if !is_transient {
            self.old_log_energy2[..state_len].copy_from_slice(&self.old_log_energy[..state_len]);
            self.old_log_energy[..state_len].copy_from_slice(&self.prev_energy[..state_len]);
        } else {
            for i in 0..state_len {
                self.old_log_energy[i] = self.old_log_energy[i].min(self.prev_energy[i]);
            }
        }
        // Tras una pérdida, el fondo puede subir lo que habría subido en todos los bloques
        // perdidos, no solo en los de esta trama.
        let blocks = self.loss_duration as usize + (1usize << lm);
        let max_background_increase =
            (blocks.min(BACKGROUND_MAX_INCREASE_BLOCKS) as f32) * BACKGROUND_INCREASE_PER_BLOCK;
        for i in 0..state_len {
            self.background_log_energy[i] =
                (self.background_log_energy[i] + max_background_increase).min(self.prev_energy[i]);
        }
        for c in 0..2usize {
            let base = c * nb_ebands;
            for i in 0..start.min(nb_ebands) {
                self.prev_energy[base + i] = 0.0;
                self.old_log_energy[base + i] = LOG_ENERGY_FLOOR_DB;
                self.old_log_energy2[base + i] = LOG_ENERGY_FLOOR_DB;
            }
            for i in end.min(nb_ebands)..nb_ebands {
                self.prev_energy[base + i] = 0.0;
                self.old_log_energy[base + i] = LOG_ENERGY_FLOOR_DB;
                self.old_log_energy2[base + i] = LOG_ENERGY_FLOOR_DB;
            }
        }
    }
}

/// Parámetros de un filtro peine de tono: periodo, ganancia y juego de coeficientes.
#[derive(Debug, Clone, Copy)]
struct PitchTaps {
    period: usize,
    gain: f32,
    tapset: usize,
}

/// Postfiltro de una trama para un canal (`comb_filter` dos veces en `celt_decode_with_ec`):
/// en el primer bloque corto se pasa de los parámetros anteriores a los actuales y, en el resto
/// de la trama, de los actuales a los recién decodificados.
struct CombFilterPass {
    out_start: usize,
    frame_size_48k: usize,
    lm: usize,
    short_n: usize,
    old: PitchTaps,
    current: PitchTaps,
    new: PitchTaps,
    window: &'static [f32],
    overlap: usize,
}

impl CombFilterPass {
    /// Aplica el postfiltro a la memoria de síntesis de un canal.
    fn apply(&self, mem: &mut [f32]) {
        CeltDecoder::comb_filter_in_place(
            mem,
            self.out_start,
            self.short_n,
            self.old,
            self.current,
            self.window,
            self.overlap,
        );
        if self.lm != 0 && self.frame_size_48k > self.short_n {
            CeltDecoder::comb_filter_in_place(
                mem,
                self.out_start + self.short_n,
                self.frame_size_48k - self.short_n,
                self.current,
                self.new,
                self.window,
                self.overlap,
            );
        }
    }
}

/// Geometría de una trama CELT, común a todas las etapas de `decode_frame_with_ec`.
#[derive(Debug, Clone, Copy)]
struct FrameLayout {
    /// Primera y última banda (excluida) que se decodifican.
    start: usize,
    end: usize,
    /// Logaritmo en base 2 del número de bloques cortos de la trama.
    lm: usize,
    /// Canales codificados en el paquete: 1 o 2.
    coded_channels: usize,
    /// Duración de la trama en muestras a 48 kHz.
    frame_size_48k: usize,
    /// Factor de diezmado de 48 kHz a la frecuencia de salida.
    downsample: usize,
    /// Bits disponibles en la trama.
    total_bits: i32,
}

/// Parámetros del postfiltro de tono leídos de la cabecera.
#[derive(Debug, Clone, Copy)]
struct PostfilterParams {
    pitch: i32,
    tapset: i32,
    gain: f32,
}

/// Cabecera de una trama CELT.
#[derive(Debug, Clone, Copy)]
struct FrameHeader {
    silence: bool,
    postfilter: PostfilterParams,
    is_transient: bool,
    intra_ener: bool,
}

/// Lo que la asignación de bits deja para decodificar las bandas.
struct FrameAllocation {
    tf_res: Vec<i32>,
    spread_decision: i32,
    alloc: rate::AllocationResult,
    anti_collapse_rsv: i32,
}

/// Bandas normalizadas de cada canal codificado y sus máscaras de colapso.
struct BandShapes {
    mdct_in: Vec<f32>,
    mdct_side: Option<Vec<f32>>,
    collapse_masks: Vec<u8>,
}

/// Lee la cabecera de una trama CELT: silencio, postfiltro, transitorio y energía intra
/// (`celt_decode_with_ec`, hasta `unquant_coarse_energy`).
fn decode_frame_header(ec: &mut EcDec<'_>, layout: &FrameLayout) -> FrameHeader {
    let FrameLayout {
        start,
        lm,
        total_bits,
        ..
    } = *layout;
    let mut tell = ec.tell();
    let silence_flag = if tell >= total_bits {
        true
    } else if tell == 1 {
        ec.dec_bit_logp(SILENCE_LOGP)
    } else {
        false
    };
    if silence_flag {
        tell = total_bits;
    }

    let mut postfilter_pitch = 0i32;
    let mut postfilter_tapset = 0i32;
    let mut postfilter_gain = 0.0f32;
    if start == 0 && tell + POSTFILTER_MIN_BITS <= total_bits && ec.dec_bit_logp(1) {
        let octave = ec.dec_uint(POSTFILTER_OCTAVES) as i32;
        postfilter_pitch = ((POSTFILTER_PITCH_BASE << octave)
            + ec.dec_bits((POSTFILTER_PITCH_EXTRA_BITS + octave) as u32) as i32)
            - 1;
        let postfilter_qg = ec.dec_bits(POSTFILTER_GAIN_BITS) as i32;
        if ec.tell() + 2 <= total_bits {
            postfilter_tapset = ec.dec_icdf(&TAPSET_ICDF, 2);
        }
        postfilter_gain = POSTFILTER_GAIN_STEP * (postfilter_qg + 1) as f32;
    }
    tell = ec.tell();
    let is_transient = if lm > 0 && tell + TRANSIENT_LOGP as i32 <= total_bits {
        ec.dec_bit_logp(TRANSIENT_LOGP)
    } else {
        false
    };
    tell = ec.tell();
    let intra_ener = tell + INTRA_LOGP as i32 <= total_bits && ec.dec_bit_logp(INTRA_LOGP);

    FrameHeader {
        silence: silence_flag,
        postfilter: PostfilterParams {
            pitch: postfilter_pitch,
            tapset: postfilter_tapset,
            gain: postfilter_gain,
        },
        is_transient,
        intra_ener,
    }
}

/// LM de una trama CELT a 48 kHz: el logaritmo en base 2 de su número de bloques MDCT cortos
/// (RFC 6716 §4.3). `None` si no es una duración CELT (2,5, 5, 10 o 20 ms).
fn lm_for_frame_size(frame_size_48k: usize) -> Option<usize> {
    CELT_FRAME_SAMPLES_48K
        .iter()
        .position(|&samples| samples == frame_size_48k)
}

/// Decodifica los indicadores tiempo-frecuencia de las bandas `start..end` en `tf_res`
/// (`tf_decode`): uno por banda mientras quede presupuesto en `total_bits`, el selector si
/// cambia algo, y la traducción con `tf_select_table`.
fn tf_decode(
    start: usize,
    end: usize,
    is_transient: bool,
    tf_res: &mut [i32],
    lm: usize,
    total_bits: i32,
    dec: &mut EcDec<'_>,
) {
    let mut budget = total_bits.max(0);
    let mut tell = dec.tell();
    let mut logp = if is_transient {
        TF_FIRST_LOGP_TRANSIENT
    } else {
        TF_FIRST_LOGP
    };
    let tf_select_rsv = lm > 0 && tell + logp < budget;
    if tf_select_rsv {
        budget = budget.saturating_sub(1);
    }
    let mut tf_changed = 0i32;
    let mut curr = 0i32;
    for tf in &mut tf_res[start..end] {
        if tell + logp <= budget {
            curr ^= i32::from(dec.dec_bit_logp(logp as u32));
            tell = dec.tell();
            tf_changed |= curr;
        }
        *tf = curr;
        logp = if is_transient {
            TF_LOGP_TRANSIENT
        } else {
            TF_LOGP
        };
    }
    let mut tf_select = 0i32;
    let row = TF_SELECT_TRANSIENT_STRIDE * usize::from(is_transient);
    let idx0 = row + tf_changed as usize;
    let idx1 = row + 2 + tf_changed as usize;
    if tf_select_rsv && TF_SELECT_TABLE[lm][idx0] != TF_SELECT_TABLE[lm][idx1] {
        tf_select = i32::from(dec.dec_bit_logp(1));
    }
    for t in tf_res.iter_mut().take(end).skip(start) {
        let idx = row + 2 * tf_select as usize + *t as usize;
        *t = TF_SELECT_TABLE[lm][idx] as i32;
    }
}

#[cfg(test)]
mod tests {
    use super::{CeltDecoder, CeltOutput, NBANDS_48K_20MS};

    /// Frecuencia de salida diezmada: 48 kHz entre 3, Nyquist a 8 kHz.
    const FS_16K_HZ: u32 = 16_000;
    /// Trama de 20 ms a 16 kHz.
    const FRAME_20MS_16K: usize = 320;
    /// Primera banda que empieza por encima de 8 kHz (`eBands[17]` = 40, 40 × 200 Hz).
    const FIRST_BAND_ABOVE_8K: usize = 17;
    /// Energías (log2) de la última trama, por encima del suelo para que se oculten todas.
    const LOW_ENERGY: f32 = 2.0;
    const HIGH_ENERGY: f32 = 8.0;

    /// Ocultación a 16 kHz con la energía `high` en las bandas por encima de 8 kHz.
    fn conceal_with_high_bands(high: f32) -> Vec<i16> {
        let mut dec = CeltDecoder::new(FS_16K_HZ, 1);
        let mut energy = vec![LOW_ENERGY; NBANDS_48K_20MS];
        energy[FIRST_BAND_ABOVE_8K..].fill(high);
        dec.prev_energy[..NBANDS_48K_20MS].copy_from_slice(&energy);
        let mut out = vec![0i16; FRAME_20MS_16K];
        dec.decode_lost(FRAME_20MS_16K, &mut out, CeltOutput::Replace)
            .unwrap();
        out
    }

    /// Como `celt_decode_lost` de libopus (`celt_synthesis` con `downsample`): a 16 kHz el ruido
    /// de las bandas por encima de 8 kHz se anula antes de diezmar, así que su energía no cambia
    /// la salida. Sin el recorte se replegaría por debajo de 8 kHz.
    #[test]
    fn concealment_ignores_bands_above_output_nyquist() {
        let low = conceal_with_high_bands(LOW_ENERGY);
        let high = conceal_with_high_bands(HIGH_ENERGY);

        assert!(low.iter().any(|&s| s != 0));
        assert_eq!(low, high);
    }
}

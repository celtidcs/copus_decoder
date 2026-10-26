//! Decodificador SILK.
//!
//! Decodifica la cabecera de los paquetes SILK, su información lateral y la síntesis, con una
//! implementación cercana al decodificador de referencia.

use crate::Error;
use crate::entropy::EcDec;
use crate::packet::{BITS_PER_BYTE, DURATIONS_PER_BANDWIDTH, OPUS_RATE_HZ, SILK_FRAME_SAMPLES_48K};
use tables::{
    FS_KHZ_MB, FS_KHZ_NB, FS_KHZ_WB, HZ_PER_KHZ, ICDF_FTB, MAX_NB_SUBFR, NB_SUBFR_10MS,
    NLSF_INTERP_NONE_Q2, ONE_Q16, SUB_FRAME_LENGTH_MS, TYPE_VOICED,
};

mod decode_core;
mod entropy_tables;
mod fixed;
mod gain;
mod indices;
mod lpc;
mod ltp;
mod nlsf;
mod pitch;
mod plc;
mod resampler;
mod resampler_private;
mod resampler_rom;
mod stereo;
mod tables;

const MAX_INTERNAL_FRAMES: usize = 3;
const MAX_LPC_HALVES: usize = 2;
/// Frecuencia interna de SILK para cada ancho de banda de la configuración del TOC: estrecha,
/// media, ancha y, en las híbridas superanchas, también ancha (RFC 6716 §3.1, Tabla 2).
const SILK_INTERNAL_FS_KHZ_BY_BANDWIDTH: [u32; 4] = [FS_KHZ_NB, FS_KHZ_MB, FS_KHZ_WB, FS_KHZ_WB];
const MAX_API_FRAME_SAMPLES_48K: usize = 960 * MAX_INTERNAL_FRAMES;

/// Muestras de PCM interno que caben en un paquete: tres tramas internas de la mayor longitud.
const INTERNAL_PCM_LEN: usize = MAX_INTERNAL_FRAMES * tables::MAX_FRAME_LENGTH;

/// Resultado de decodificar una trama Opus solo SILK.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SilkFrameDecode {
    /// Muestras decodificadas por canal de salida.
    pub samples_per_channel: usize,
    /// Si la trama lleva una cola de redundancia CELT.
    pub consumed_redundancy: bool,
    /// Bytes del final de la trama que ocupa la redundancia CELT.
    pub redundancy_bytes: usize,
    /// Sentido de la transición que anuncia la cola de redundancia (de CELT a SILK).
    pub celt_to_silk: bool,
}

/// Lo que el TOC dice de una trama SILK: su duración, su configuración y los canales codificados.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SilkFrameRequest {
    /// Duración de la trama, en muestras a 48 kHz.
    pub frame_samples_48k: usize,
    /// Configuración del TOC, de la que sale la frecuencia interna.
    pub config: u8,
    /// Canales codificados en el paquete (1 o 2).
    pub packet_channels: u8,
}

#[derive(Debug, Clone, Copy, Default)]
struct ChannelHeader {
    vad_flags: [bool; MAX_INTERNAL_FRAMES],
    lbrr_flags: [bool; MAX_INTERNAL_FRAMES],
}

#[derive(Debug, Clone, Copy, Default)]
struct ParsedHeader {
    channels: [ChannelHeader; 2],
}

/// Canales codificados del paquete y longitud de sus tramas internas.
#[derive(Debug, Clone, Copy)]
struct FrameLayout {
    packet_channels: usize,
    frame_length: usize,
}

/// Una trama interna de un canal: su posición en el paquete, su bit de actividad de voz, su
/// codificación condicional y su longitud.
#[derive(Debug, Clone, Copy)]
struct InternalFrame {
    index: usize,
    vad: bool,
    cond: indices::CondCoding,
    frame_length: usize,
}

/// PCM interno de un paquete por canal, en tramas internas de `frame_length` muestras.
struct InternalPcm<'a> {
    channels: [&'a [i16]; 2],
    packet_channels: usize,
    frame_length: usize,
}

/// Estado persistente de un canal codificado de SILK: índices, síntesis, remuestreo y
/// coeficientes LPC de las tramas internas del último paquete.
#[derive(Debug, Clone)]
struct SilkChannel {
    /// Estado de la lectura de índices (frecuencia interna, subtramas y valores previos).
    indices: indices::ChannelState,
    /// Estado de la síntesis y de la ocultación de pérdidas.
    core: decode_core::SilkChannelState,
    /// Remuestreador a la frecuencia de la API; se crea con la primera trama a 48 kHz.
    resampler: Option<resampler::SilkResampler>,
    /// NLSF de la trama interna anterior, para interpolar (`prevNLSF_Q15`).
    prev_nlsf_q15: [i16; tables::MAX_LPC_ORDER],
    /// Coeficientes LPC de cada mitad de cada trama interna del último paquete.
    pred_coef_q12: [[[i16; tables::MAX_LPC_ORDER]; MAX_LPC_HALVES]; MAX_INTERNAL_FRAMES],
    /// Sin trama anterior desde el último reinicio: no se interpolan las NLSF.
    first_frame_after_reset: bool,
}

impl Default for SilkChannel {
    fn default() -> Self {
        Self {
            indices: indices::ChannelState::default(),
            core: decode_core::SilkChannelState::default(),
            resampler: None,
            prev_nlsf_q15: [0; tables::MAX_LPC_ORDER],
            pred_coef_q12: [[[0; tables::MAX_LPC_ORDER]; MAX_LPC_HALVES]; MAX_INTERNAL_FRAMES],
            first_frame_after_reset: true,
        }
    }
}

impl SilkChannel {
    /// Fija la frecuencia interna `fs_khz` y las subtramas de la trama. Si la frecuencia cambia,
    /// se reinicia la síntesis y la siguiente trama no interpola las NLSF.
    fn set_internal_rate(&mut self, fs_khz: u32, nb_subfr: usize) {
        let prev_fs_khz = self.indices.fs_khz;
        if prev_fs_khz != 0 && prev_fs_khz != fs_khz {
            self.core = decode_core::SilkChannelState::default();
            self.first_frame_after_reset = true;
        }
        indices::configure_channel_state(&mut self.indices, fs_khz, nb_subfr);
    }

    /// Crea el remuestreador de `internal_fs_hz` a `api_fs_hz` si no existe o convierte otras
    /// frecuencias.
    ///
    /// # Errores
    ///
    /// Los de `SilkResampler::new` si la pareja de frecuencias no está admitida.
    fn ensure_resampler(&mut self, internal_fs_hz: u32, api_fs_hz: u32) -> Result<(), Error> {
        let matches = self
            .resampler
            .as_ref()
            .is_some_and(|state| state.matches(internal_fs_hz, api_fs_hz));
        if !matches {
            self.resampler = Some(resampler::SilkResampler::new(internal_fs_hz, api_fs_hz)?);
        }
        Ok(())
    }

    /// Decodifica la trama interna `frame` de este canal en `pcm`: índices, coeficientes LPC,
    /// pulsos y síntesis. Devuelve lo que necesita la ocultación cuando la trama ya esté en
    /// izquierdo y derecho.
    ///
    /// # Errores
    ///
    /// `BadPacket` si los índices o los pulsos no se pueden decodificar.
    fn decode_internal_frame(
        &mut self,
        dec: &mut EcDec<'_>,
        frame: InternalFrame,
        pcm: &mut [i16],
    ) -> Result<PlcUpdate, Error> {
        let side = indices::decode_indices(&mut self.indices, dec, frame.vad, false, frame.cond)?;
        let fs_khz = self.indices.fs_khz;
        let pred_coef_q12 = &mut self.pred_coef_q12[frame.index];
        update_lpc_from_nlsf(
            fs_khz,
            &side.nlsf,
            &mut self.prev_nlsf_q15,
            pred_coef_q12,
            &mut self.first_frame_after_reset,
        );
        let pulses = indices::decode_pulses(
            dec,
            side.signal_type,
            side.quant_offset_type,
            frame.frame_length,
        )?;
        let params = decode_core::SilkFrameParams {
            signal_type: side.signal_type,
            quant_offset_type: side.quant_offset_type,
            gain_indices: side.gain_indices,
            lag_index: side.lag_index,
            contour_index: side.contour_index,
            ltp_periodicity: side.ltp_periodicity,
            ltp_indices: side.ltp_indices,
            ltp_scale_index: side.ltp_scale_index,
            seed: side.seed,
            pulses,
            conditional: matches!(frame.cond, indices::CondCoding::Conditionally),
            frame_length: frame.frame_length,
            nb_subfr: self.indices.nb_subfr,
            fs_khz,
            lpc_order: tables::nlsf_codebook(fs_khz).order,
        };
        decode_core::decode_core(&params, pred_coef_q12, pcm, &mut self.core);
        Ok(PlcUpdate::new(&params, &pred_coef_q12[1]))
    }

    /// Actualiza el estado de ocultación con la trama interna `pcm` ya decodificada.
    fn update_plc(&mut self, update: &PlcUpdate, pcm: &[i16]) {
        let decoded = plc::SilkDecodedFrame {
            lag: update.lag,
            ltp_gains: update.ltp_gains,
            output: pcm.to_vec(),
        };
        let nb_subfr = self.indices.nb_subfr;
        plc::plc_update(
            &mut self.core.plc,
            &decoded,
            &update.lpc_q12[..update.lpc_order],
            nb_subfr,
            pcm.len() / nb_subfr,
            self.indices.fs_khz as i32,
            update.lpc_order,
        );
    }

    /// Oculta una trama interna perdida en `frame` y la añade al historial de síntesis.
    fn conceal(&mut self, frame: &mut [i16], loss_count: u32) {
        let core = &mut self.core;
        plc::plc_conceal(
            &mut core.plc,
            &mut core.s_lpc_q14_buf[..],
            frame,
            loss_count,
        );
        core.record_concealed_frame(frame);
    }
}

/// Lo que la ocultación necesita de una trama interna decodificada: el lag de tono y las
/// ganancias LTP (cero en tramas no sonoras) y los coeficientes LPC de su segunda mitad.
struct PlcUpdate {
    lag: i32,
    ltp_gains: [i32; MAX_NB_SUBFR],
    lpc_q12: [i32; tables::MAX_LPC_ORDER],
    lpc_order: usize,
}

impl PlcUpdate {
    /// Toma de `params` el tono y de `lpc_half` los coeficientes de la segunda mitad.
    fn new(params: &decode_core::SilkFrameParams, lpc_half: &[i16; tables::MAX_LPC_ORDER]) -> Self {
        let (lag, ltp_gains) = if params.signal_type == TYPE_VOICED {
            voiced_pitch(params)
        } else {
            (0, [0; MAX_NB_SUBFR])
        };
        let mut lpc_q12 = [0i32; tables::MAX_LPC_ORDER];
        for (dst, &src) in lpc_q12.iter_mut().zip(lpc_half).take(params.lpc_order) {
            *dst = i32::from(src);
        }
        Self {
            lag,
            ltp_gains,
            lpc_q12,
            lpc_order: params.lpc_order,
        }
    }
}

/// Lag de tono de la última subtrama de una trama sonora y ganancia LTP de cada subtrama: la
/// suma de los coeficientes de su filtro en Q14, acotada a `0..=ONE_Q16`.
fn voiced_pitch(params: &decode_core::SilkFrameParams) -> (i32, [i32; MAX_NB_SUBFR]) {
    let pitch_lags = pitch::decode_pitch_lags(
        params.lag_index as i32,
        params.contour_index as i32,
        params.fs_khz as i32,
        params.nb_subfr,
    );
    let (ltp_coeffs_q14, _) = ltp::decode_ltp_coeffs(
        params.ltp_periodicity,
        &params.ltp_indices,
        params.nb_subfr,
        params.ltp_scale_index,
    );
    let mut ltp_gains = [0i32; MAX_NB_SUBFR];
    for (gain, coeffs) in ltp_gains
        .iter_mut()
        .zip(&ltp_coeffs_q14)
        .take(params.nb_subfr)
    {
        let summed_q14 = coeffs
            .iter()
            .map(|coef| i32::from(*coef))
            .sum::<i32>()
            .max(0);
        *gain = summed_q14.clamp(0, ONE_Q16);
    }
    (pitch_lags[params.nb_subfr - 1], ltp_gains)
}

/// Decodificador SILK con estado, usado por el decodificador Opus.
#[derive(Debug, Clone)]
pub(crate) struct SilkDecoder {
    fs_hz: u32,
    channels: u8,
    final_range: u32,
    channel_state: [SilkChannel; 2],
    stereo_state: stereo::StereoState,
    prev_decode_only_middle: bool,
    prev_packet_channels: u8,
}

impl SilkDecoder {
    /// Crea el decodificador SILK para la frecuencia de salida `fs_hz` y `channels` canales de
    /// salida, sin estado previo.
    pub fn new(fs_hz: u32, channels: u8) -> Self {
        Self {
            fs_hz,
            channels,
            final_range: 0,
            channel_state: Default::default(),
            stereo_state: stereo::StereoState::default(),
            prev_decode_only_middle: false,
            prev_packet_channels: 1,
        }
    }

    /// Vuelve al estado recién creado, conservando la frecuencia y los canales de salida.
    pub fn reset(&mut self) {
        *self = Self::new(self.fs_hz, self.channels);
    }

    /// Estado final del decodificador de rango tras la última trama, para comprobar la
    /// conformidad.
    pub fn final_range(&self) -> u32 {
        self.final_range
    }

    /// SILK solo remuestrea si la salida es de 48 kHz; a otra frecuencia copia el PCM interno tal
    /// cual (heredado del original).
    fn resamples(&self) -> bool {
        self.fs_hz as usize == OPUS_RATE_HZ
    }

    /// Muestras de cada trama interna ocultada: las de la última trama decodificada del canal 0,
    /// o `None` si todavía no se ha decodificado ninguna.
    fn concealment_frame_samples(&self) -> Option<usize> {
        let plc = &self.channel_state[0].core.plc;
        let samples = plc.subfr_length.saturating_mul(plc.nb_subfr);
        (samples != 0 && plc.fs_khz > 0).then_some(samples)
    }

    /// Muestras por canal de salida que da una trama interna de `internal_samples` muestras.
    fn output_len(&self, internal_samples: usize) -> usize {
        if !self.resamples() {
            return internal_samples;
        }
        self.channel_state[0]
            .resampler
            .as_ref()
            .map_or(0, |resampler| resampler.output_len(internal_samples))
    }

    /// Oculta un paquete SILK o híbrido perdido con el estado de ocultación guardado.
    ///
    /// Escribe en `out` `samples_per_channel` muestras por canal, entrelazadas, y las devuelve.
    /// Sin trama anterior que imitar, la salida es silencio.
    ///
    /// # Errores
    ///
    /// `OutputTooSmall` si `out` no tiene sitio para todas las muestras.
    pub(crate) fn decode_lost(
        &mut self,
        samples_per_channel: usize,
        out: &mut [i16],
        loss_count: u32,
    ) -> Result<usize, Error> {
        let packet_channels = self.prev_packet_channels.clamp(1, 2) as usize;
        let needed = samples_per_channel * self.channels as usize;
        if out.len() < needed {
            return Err(Error::OutputTooSmall {
                needed,
                got: out.len(),
            });
        }
        out[..needed].fill(0);
        let Some(frame_samples) = self.concealment_frame_samples() else {
            return Ok(samples_per_channel);
        };
        let frame_output_samples = self.output_len(frame_samples);
        if frame_output_samples == 0 {
            return Ok(samples_per_channel);
        }

        let mut mono_history = self.stereo_state.s_mid;
        let mut written_total = 0usize;
        while written_total < samples_per_channel {
            let internal_pcm =
                self.conceal_internal_frame(packet_channels, frame_samples, loss_count);
            let frame_write = frame_output_samples.min(samples_per_channel - written_total);
            let pcm = InternalPcm {
                channels: [
                    &internal_pcm[0][..frame_samples],
                    &internal_pcm[1][..frame_samples],
                ],
                packet_channels,
                frame_length: frame_samples,
            };
            self.emit(&pcm, &mut mono_history, out, written_total, frame_write);
            written_total += frame_write;
        }
        Ok(samples_per_channel)
    }

    /// Oculta una trama interna de `frame_samples` muestras en cada canal del último paquete.
    fn conceal_internal_frame(
        &mut self,
        packet_channels: usize,
        frame_samples: usize,
        loss_count: u32,
    ) -> [[i16; tables::MAX_FRAME_LENGTH]; 2] {
        let mut pcm = [[0i16; tables::MAX_FRAME_LENGTH]; 2];
        for (channel, frame_pcm) in self
            .channel_state
            .iter_mut()
            .zip(pcm.iter_mut())
            .take(packet_channels)
        {
            channel.conceal(&mut frame_pcm[..frame_samples], loss_count);
        }
        if packet_channels == 1 {
            self.stereo_state
                .s_mid
                .copy_from_slice(&pcm[0][frame_samples - 2..frame_samples]);
        }
        pcm
    }

    /// Escribe en `out`, desde la muestra por canal `first`, `count` muestras por canal del PCM
    /// interno `pcm`, remuestreado si la salida es de 48 kHz. Con un paquete mono, el remuestreo
    /// usa y actualiza `mono_history`.
    fn emit(
        &mut self,
        pcm: &InternalPcm<'_>,
        mono_history: &mut [i16; 2],
        out: &mut [i16],
        first: usize,
        count: usize,
    ) {
        let stereo_packet = pcm.packet_channels == 2;
        let stereo_out = self.channels == 2;
        if !self.resamples() {
            let copy_len = pcm.channels[0].len().min(count);
            write_interleaved(
                out,
                first,
                &pcm.channels[0][..copy_len],
                stereo_packet.then_some(&pcm.channels[1][..copy_len]),
                stereo_out,
            );
            return;
        }
        let mut resampled = [[0i16; MAX_API_FRAME_SAMPLES_48K]; 2];
        for (ch, channel) in self
            .channel_state
            .iter_mut()
            .enumerate()
            .take(pcm.packet_channels)
        {
            if let Some(resampler) = channel.resampler.as_mut() {
                let history = (!stereo_packet).then_some(&mut *mono_history);
                resample_frames(
                    resampler,
                    pcm.channels[ch],
                    pcm.frame_length,
                    &mut resampled[ch],
                    history,
                );
            }
        }
        write_interleaved(
            out,
            first,
            &resampled[0][..count],
            stereo_packet.then_some(&resampled[1][..count]),
            stereo_out,
        );
    }

    /// Decodifica una trama Opus solo SILK y su posible cola de redundancia CELT.
    ///
    /// # Errores
    ///
    /// Los de [`Self::decode_frame_with_ec`].
    pub fn decode_frame(
        &mut self,
        frame: &[u8],
        request: SilkFrameRequest,
        out: &mut [i16],
    ) -> Result<SilkFrameDecode, Error> {
        let mut dec = EcDec::new(frame);
        let samples_per_channel = self.decode_payload(&mut dec, request, out)?;
        let trailer = read_redundancy_trailer(&mut dec, frame);
        self.finish(&dec)?;
        Ok(SilkFrameDecode {
            samples_per_channel,
            ..trailer
        })
    }

    /// Decodifica la parte SILK de una trama híbrida con el decodificador de rango compartido
    /// `dec`, que la capa CELT sigue leyendo después. Devuelve las muestras por canal de salida.
    ///
    /// # Errores
    ///
    /// `BadPacket` si la trama no es SILK válida, `OutputTooSmall` si `out` no tiene sitio y los
    /// de la creación del remuestreador.
    pub(crate) fn decode_frame_with_ec(
        &mut self,
        dec: &mut EcDec<'_>,
        request: SilkFrameRequest,
        out: &mut [i16],
    ) -> Result<usize, Error> {
        let samples_per_channel = self.decode_payload(dec, request, out)?;
        self.finish(dec)?;
        Ok(samples_per_channel)
    }

    /// Guarda el estado final del decodificador de rango.
    ///
    /// # Errores
    ///
    /// `BadPacket` si la lectura se salió de la trama.
    fn finish(&mut self, dec: &EcDec<'_>) -> Result<(), Error> {
        self.final_range = dec.final_range();
        if dec.is_error() {
            return Err(Error::BadPacket);
        }
        Ok(())
    }

    /// Decodifica las tramas internas del paquete y escribe su PCM en `out`. No toca el estado
    /// final del decodificador de rango, que se guarda al terminar la trama.
    ///
    /// # Errores
    ///
    /// Los de [`Self::decode_frame_with_ec`] salvo el de lectura fuera de la trama.
    fn decode_payload(
        &mut self,
        dec: &mut EcDec<'_>,
        request: SilkFrameRequest,
        out: &mut [i16],
    ) -> Result<usize, Error> {
        let SilkFrameRequest {
            frame_samples_48k,
            config,
            packet_channels,
        } = request;
        if !matches!(packet_channels, 1 | 2) {
            return Err(Error::BadPacket);
        }
        let packet_channels = usize::from(packet_channels);
        let internal_fs_hz = silk_internal_fs_hz(config)?;
        let (internal_frames, nb_subfr, frame_length) =
            internal_frame_shape(frame_samples_48k, internal_fs_hz)?;
        let mut mono_history = self.stereo_state.s_mid;
        let samples_per_channel = (frame_samples_48k * self.fs_hz as usize) / OPUS_RATE_HZ;
        let needed = samples_per_channel * self.channels as usize;
        if out.len() < needed {
            return Err(Error::OutputTooSmall {
                needed,
                got: out.len(),
            });
        }

        self.prepare_channels(packet_channels, internal_fs_hz, nb_subfr)?;
        let header = parse_header(dec, packet_channels, internal_frames)?;
        consume_lbrr_payload(
            dec,
            &header,
            packet_channels,
            internal_frames,
            &mut self.channel_state,
            frame_length,
        )?;
        let layout = FrameLayout {
            packet_channels,
            frame_length,
        };
        let mut internal_pcm = [[0i16; INTERNAL_PCM_LEN]; 2];
        for i in 0..internal_frames {
            self.decode_internal_frame(dec, &header, layout, i, &mut internal_pcm)?;
        }

        out[..needed].fill(0);
        let internal_samples = internal_frames * frame_length;
        let pcm = InternalPcm {
            channels: [
                &internal_pcm[0][..internal_samples],
                &internal_pcm[1][..internal_samples],
            ],
            packet_channels,
            frame_length,
        };
        self.emit(&pcm, &mut mono_history, out, 0, samples_per_channel);
        self.prev_packet_channels = packet_channels as u8;
        Ok(samples_per_channel)
    }

    /// Prepara los canales del paquete para la frecuencia interna `internal_fs_hz` y, a 48 kHz,
    /// sus remuestreadores. Al pasar de un paquete mono a uno estéreo con salida estéreo, se
    /// borra la predicción estéreo y el canal derecho parte del remuestreador del izquierdo.
    ///
    /// # Errores
    ///
    /// Los de la creación del remuestreador.
    fn prepare_channels(
        &mut self,
        packet_channels: usize,
        internal_fs_hz: u32,
        nb_subfr: usize,
    ) -> Result<(), Error> {
        let fs_khz = internal_fs_hz / HZ_PER_KHZ;
        for channel in self.channel_state.iter_mut().take(packet_channels) {
            channel.set_internal_rate(fs_khz, nb_subfr);
        }
        if !self.resamples() {
            return Ok(());
        }
        let api_fs_hz = self.fs_hz;
        for channel in self.channel_state.iter_mut().take(packet_channels) {
            channel.ensure_resampler(internal_fs_hz, api_fs_hz)?;
        }
        if self.channels == 2 && packet_channels == 2 && self.prev_packet_channels == 1 {
            self.stereo_state.reset_mono_to_stereo();
            if let Some(left_resampler) = self.channel_state[0].resampler.clone() {
                self.channel_state[1].resampler = Some(left_resampler);
            }
        }
        Ok(())
    }

    /// Decodifica la trama interna `i` de los canales del paquete en `internal_pcm`: predicción
    /// estéreo, cada canal, la conversión de medio y lateral a izquierdo y derecho y la
    /// actualización de la ocultación.
    ///
    /// # Errores
    ///
    /// `BadPacket` si los índices o los pulsos de un canal no se pueden decodificar.
    fn decode_internal_frame(
        &mut self,
        dec: &mut EcDec<'_>,
        header: &ParsedHeader,
        layout: FrameLayout,
        i: usize,
        internal_pcm: &mut [[i16; INTERNAL_PCM_LEN]; 2],
    ) -> Result<(), Error> {
        let FrameLayout {
            packet_channels,
            frame_length,
        } = layout;
        let range = i * frame_length..(i + 1) * frame_length;
        let mut pred_q13 = [0i32; 2];
        let mut decode_only_middle = false;
        if packet_channels == 2 {
            pred_q13 = stereo::decode_stereo_pred(dec);
            decode_only_middle = !header.channels[1].vad_flags[i] && stereo::decode_mid_only(dec);
        }

        let mut pending: [Option<PlcUpdate>; 2] = [None, None];
        for ch in 0..packet_channels {
            if ch == 1 && decode_only_middle {
                continue;
            }
            let frame = InternalFrame {
                index: i,
                vad: header.channels[ch].vad_flags[i],
                cond: main_cond_coding(i, ch, self.prev_decode_only_middle),
                frame_length,
            };
            pending[ch] = Some(self.channel_state[ch].decode_internal_frame(
                dec,
                frame,
                &mut internal_pcm[ch][range.clone()],
            )?);
        }

        if packet_channels == 2 {
            let [left, right] = &mut *internal_pcm;
            stereo::ms_to_lr(
                &mut self.stereo_state,
                &mut left[range.clone()],
                &mut right[range.clone()],
                pred_q13,
                self.channel_state[0].indices.fs_khz,
            );
        } else {
            self.stereo_state
                .s_mid
                .copy_from_slice(&internal_pcm[0][range.end - 2..range.end]);
        }

        for ((channel, update), pcm) in self
            .channel_state
            .iter_mut()
            .zip(&pending)
            .zip(internal_pcm.iter())
        {
            if let Some(update) = update {
                channel.update_plc(update, &pcm[range.clone()]);
            }
        }
        self.prev_decode_only_middle = decode_only_middle;
        Ok(())
    }
}

/// Codificación condicional de la trama interna `i` del canal `ch`: el canal lateral que sigue a
/// una trama solo de medio no escala el LTP; la primera trama del paquete no se condiciona y las
/// demás se condicionan a la anterior.
fn main_cond_coding(i: usize, ch: usize, prev_decode_only_middle: bool) -> indices::CondCoding {
    if ch > 0 && prev_decode_only_middle {
        indices::CondCoding::IndependentlyNoLtpScaling
    } else if i == 0 {
        indices::CondCoding::Independently
    } else {
        indices::CondCoding::Conditionally
    }
}

/// Lee la cola de redundancia de una trama solo SILK (RFC 6716 §4.5.1): un bit de sentido y,
/// en los bytes que queden, la trama CELT redundante. Sin bits suficientes no hay redundancia.
fn read_redundancy_trailer(dec: &mut EcDec<'_>, frame: &[u8]) -> SilkFrameDecode {
    let mut trailer = SilkFrameDecode {
        samples_per_channel: 0,
        consumed_redundancy: false,
        redundancy_bytes: 0,
        celt_to_silk: false,
    };
    if dec.tell() + crate::decoder::REDUNDANCY_MIN_BITS <= (BITS_PER_BYTE * frame.len()) as i32 {
        trailer.celt_to_silk = dec.dec_bit_logp(1);
        trailer.redundancy_bytes = frame
            .len()
            .saturating_sub((dec.tell().max(0) as usize).div_ceil(BITS_PER_BYTE));
        trailer.consumed_redundancy = trailer.redundancy_bytes > 0;
    }
    trailer
}

/// Remuestrea `pcm` trama interna a trama interna de `frame_length` muestras en `out`. Con
/// `mono_history`, cada trama se retrasa una muestra (ver [`resample_delayed`]).
fn resample_frames(
    resampler: &mut resampler::SilkResampler,
    pcm: &[i16],
    frame_length: usize,
    out: &mut [i16],
    mut mono_history: Option<&mut [i16; 2]>,
) {
    let mut written_total = 0usize;
    for frame in pcm.chunks_exact(frame_length) {
        let expected = resampler.output_len(frame_length);
        let target = &mut out[written_total..written_total + expected];
        let written = match mono_history.as_mut() {
            Some(history) => resample_delayed(resampler, frame, target, history),
            None => resampler.process(frame, target),
        };
        debug_assert_eq!(written, expected);
        written_total += written;
    }
}

/// Remuestrea una trama interna de un paquete mono retrasada una muestra: la entrada empieza en
/// `history[1]`, la última muestra de la trama anterior, como en `silk_Decode` de libopus, que
/// remuestrea desde `&samplesOut1_tmp[n][1]`. Guarda en `history` las dos últimas muestras.
fn resample_delayed(
    resampler: &mut resampler::SilkResampler,
    frame: &[i16],
    out: &mut [i16],
    history: &mut [i16; 2],
) -> usize {
    let n = frame.len();
    let mut delayed = Vec::with_capacity(n);
    delayed.push(history[1]);
    delayed.extend_from_slice(&frame[..n - 1]);
    let written = resampler.process(&delayed, out);
    history.copy_from_slice(&frame[n - 2..]);
    written
}

/// Lee con el decodificador de rango compartido `dec` la cabecera del paquete SILK, previa a las
/// tramas internas: para cada uno de los `packet_channels` canales codificados, el bit de
/// actividad de voz de cada una de las `internal_frames` tramas internas (1, 2 o 3) y los
/// indicadores LBRR. Devuelve esos indicadores; falla si no se pueden leer los de LBRR.
fn parse_header(
    dec: &mut EcDec<'_>,
    packet_channels: usize,
    internal_frames: usize,
) -> Result<ParsedHeader, Error> {
    let mut parsed = ParsedHeader::default();

    for ch in 0..packet_channels {
        for i in 0..internal_frames {
            parsed.channels[ch].vad_flags[i] = dec.dec_bit_logp(1);
        }

        let has_lbrr = dec.dec_bit_logp(1);
        parsed.channels[ch].lbrr_flags = unpack_lbrr_flags(dec, has_lbrr, internal_frames)?;
    }

    Ok(parsed)
}

/// Lee y descarta de `dec` las tramas LBRR que marca `header` en cada canal codificado: la
/// predicción estéreo, el indicador de solo medio, los índices y los pulsos de tramas internas
/// de `frame_length` muestras. Los índices se leen con el estado de `states`, que se actualiza.
/// Falla con `BadPacket` si los índices o los pulsos no se pueden decodificar.
fn consume_lbrr_payload(
    dec: &mut EcDec<'_>,
    header: &ParsedHeader,
    packet_channels: usize,
    internal_frames: usize,
    states: &mut [SilkChannel; 2],
    frame_length: usize,
) -> Result<(), Error> {
    for i in 0..internal_frames {
        for (ch, state) in states.iter_mut().enumerate().take(packet_channels) {
            if !header.channels[ch].lbrr_flags[i] {
                continue;
            }
            if packet_channels == 2 && ch == 0 {
                let _ = stereo::decode_stereo_pred(dec);
                if !header.channels[1].lbrr_flags[i] {
                    let _ = stereo::decode_mid_only(dec);
                }
            }
            let cond = if i > 0 && header.channels[ch].lbrr_flags[i - 1] {
                indices::CondCoding::Conditionally
            } else {
                indices::CondCoding::Independently
            };
            let side = indices::decode_indices(
                &mut state.indices,
                dec,
                header.channels[ch].vad_flags[i],
                true,
                cond,
            )?;
            let _ = indices::decode_pulses(
                dec,
                side.signal_type,
                side.quant_offset_type,
                frame_length,
            )?;
        }
    }
    Ok(())
}

/// Decodifica las NLSF de `nlsf_indices` con el libro de `fs_khz` y las convierte en los
/// coeficientes LPC de la segunda mitad de `pred_coef_q12`. Los de la primera mitad salen de
/// interpolar con `prev_nlsf_q15`, salvo que no haya interpolación o sea la primera trama tras
/// un reinicio (`first_frame_after_reset`); entonces copian los de la segunda. Al terminar
/// guarda las NLSF actuales en `prev_nlsf_q15` y borra `first_frame_after_reset`.
fn update_lpc_from_nlsf(
    fs_khz: u32,
    nlsf_indices: &indices::DecodedNlsfIndices,
    prev_nlsf_q15: &mut [i16; tables::MAX_LPC_ORDER],
    pred_coef_q12: &mut [[i16; tables::MAX_LPC_ORDER]; MAX_LPC_HALVES],
    first_frame_after_reset: &mut bool,
) {
    let codebook = tables::nlsf_codebook(fs_khz);
    let order = codebook.order;
    let mut curr_nlsf_q15 = [0i16; tables::MAX_LPC_ORDER];
    nlsf::nlsf_decode(
        &mut curr_nlsf_q15[..order],
        &nlsf_indices.values[..order + 1],
        codebook,
    );
    lpc::nlsf2a(
        &mut pred_coef_q12[1][..order],
        &curr_nlsf_q15[..order],
        codebook.lpc_ordering,
    );

    let interp_coef_q2 = if *first_frame_after_reset {
        NLSF_INTERP_NONE_Q2
    } else {
        nlsf_indices.interp_coef_q2
    };
    if interp_coef_q2 < NLSF_INTERP_NONE_Q2 {
        let mut interp_nlsf_q15 = [0i16; tables::MAX_LPC_ORDER];
        for i in 0..order {
            let delta = curr_nlsf_q15[i] as i32 - prev_nlsf_q15[i] as i32;
            interp_nlsf_q15[i] =
                (prev_nlsf_q15[i] as i32 + ((interp_coef_q2 as i32 * delta) >> 2)) as i16;
        }
        lpc::nlsf2a(
            &mut pred_coef_q12[0][..order],
            &interp_nlsf_q15[..order],
            codebook.lpc_ordering,
        );
    } else {
        let pred1 = pred_coef_q12[1];
        pred_coef_q12[0][..order].copy_from_slice(&pred1[..order]);
    }

    prev_nlsf_q15[..order].copy_from_slice(&curr_nlsf_q15[..order]);
    *first_frame_after_reset = false;
}

/// Lee de `dec` los indicadores LBRR de cada una de las `internal_frames` tramas internas de un
/// canal codificado, si `has_lbrr` dice que el paquete lleva LBRR. Con una sola trama interna no
/// se lee nada más: la trama lleva LBRR. Devuelve los indicadores en `[0..internal_frames)`;
/// falla con `BadPacket` si no hay tabla para ese número de tramas internas.
fn unpack_lbrr_flags(
    dec: &mut EcDec<'_>,
    has_lbrr: bool,
    internal_frames: usize,
) -> Result<[bool; MAX_INTERNAL_FRAMES], Error> {
    if !has_lbrr {
        return Ok([false; MAX_INTERNAL_FRAMES]);
    }

    if internal_frames == 1 {
        return Ok([true, false, false]);
    }

    let icdf = tables::lbrr_flags_icdf(internal_frames).ok_or(Error::BadPacket)?;
    let symbol = dec.dec_icdf(icdf, ICDF_FTB) as u32 + 1;
    Ok(unpack_lbrr_symbol(symbol, internal_frames))
}

/// Convierte los bits de `symbol` en los indicadores LBRR de las `internal_frames` primeras
/// tramas internas: el bit `i` es el de la trama `i`.
fn unpack_lbrr_symbol(symbol: u32, internal_frames: usize) -> [bool; MAX_INTERNAL_FRAMES] {
    let mut out = [false; MAX_INTERNAL_FRAMES];
    for (i, flag) in out.iter_mut().take(internal_frames).enumerate() {
        *flag = ((symbol >> i) & 1) != 0;
    }
    out
}

/// Frecuencia interna de SILK, en Hz (8000, 12000 o 16000), para la configuración del TOC
/// `config`, `(toc >> 3) & 0x1f`. Falla con `BadPacket` si la configuración no es solo SILK ni
/// híbrida.
fn silk_internal_fs_hz(config: u8) -> Result<u32, Error> {
    SILK_INTERNAL_FS_KHZ_BY_BANDWIDTH
        .get(usize::from(config / DURATIONS_PER_BANDWIDTH))
        .map(|fs_khz| fs_khz * HZ_PER_KHZ)
        .ok_or(Error::BadPacket)
}

/// Escribe en `out` las muestras de `left` (y de `right`), entrelazadas, a partir de la muestra
/// por canal `first`.
///
/// Con salida mono solo se escribe `left`. Con salida estéreo, `right` va al canal derecho; si el
/// paquete es mono (`right` es `None`), se duplica `left`. `Decoder::new` solo admite 1 o 2
/// canales de salida, así que no hay más casos.
fn write_interleaved(
    out: &mut [i16],
    first: usize,
    left: &[i16],
    right: Option<&[i16]>,
    stereo_out: bool,
) {
    if !stereo_out {
        out[first..first + left.len()].copy_from_slice(left);
        return;
    }
    let right = right.unwrap_or(left);
    let pairs = out[2 * first..2 * (first + left.len())].chunks_exact_mut(2);
    for ((pair, &l), &r) in pairs.zip(left).zip(right) {
        pair[0] = l;
        pair[1] = r;
    }
}

/// Forma de las tramas internas de una trama Opus de `frame_samples_48k` muestras a 48 kHz con
/// frecuencia interna `internal_fs_hz`: devuelve `(tramas internas, subtramas, muestras por trama
/// interna)`. Falla con `BadPacket` si la duración no es de SILK.
fn internal_frame_shape(
    frame_samples_48k: usize,
    internal_fs_hz: u32,
) -> Result<(usize, usize, usize), Error> {
    let fs_khz = (internal_fs_hz / HZ_PER_KHZ) as usize;
    // Las duraciones SILK son 10, 20, 40 y 60 ms: la de 10 ms es una trama interna de 2
    // subtramas, y las demás, tantas tramas internas de 20 ms como su posición.
    let position = SILK_FRAME_SAMPLES_48K
        .iter()
        .position(|&samples| samples == frame_samples_48k)
        .ok_or(Error::BadPacket)?;
    let (internal_frames, nb_subfr) = if position == 0 {
        (1, NB_SUBFR_10MS)
    } else {
        (position, MAX_NB_SUBFR)
    };
    Ok((
        internal_frames,
        nb_subfr,
        nb_subfr * SUB_FRAME_LENGTH_MS * fs_khz,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Decoder;
    use crate::test_vectors::{read_opus_demo_records, vectors_dir};

    const TV02_PKT0_LPC_Q12: [i16; 10] = [5041, -1246, 1017, -614, 431, -560, 131, -357, 284, -46];

    #[test]
    fn maps_internal_frame_counts() {
        assert_eq!(internal_frame_shape(480, 8_000).unwrap(), (1, 2, 80));
        assert_eq!(internal_frame_shape(960, 8_000).unwrap(), (1, 4, 160));
        assert_eq!(internal_frame_shape(1_920, 12_000).unwrap(), (2, 4, 240));
        assert_eq!(internal_frame_shape(2_880, 16_000).unwrap(), (3, 4, 320));
        assert!(internal_frame_shape(2_400, 12_000).is_err());
    }

    /// Versión anterior de `internal_frame_shape`, con los literales, como oráculo.
    fn internal_frame_shape_literal(
        frame_samples_48k: usize,
        internal_fs_hz: u32,
    ) -> Option<(usize, usize, usize)> {
        let fs_khz = (internal_fs_hz / 1000) as usize;
        match frame_samples_48k {
            480 => Some((1, 2, 10 * fs_khz)),
            960 => Some((1, 4, 20 * fs_khz)),
            1_920 => Some((2, 4, 20 * fs_khz)),
            2_880 => Some((3, 4, 20 * fs_khz)),
            _ => None,
        }
    }

    #[test]
    fn internal_frame_shape_matches_literal_version() {
        for fs_hz in [8_000, 12_000, 16_000] {
            for samples in 0..=crate::packet::MAX_DURATION_SAMPLES_48K {
                assert_eq!(
                    internal_frame_shape(samples, fs_hz).ok(),
                    internal_frame_shape_literal(samples, fs_hz),
                    "{samples} muestras a {fs_hz} Hz"
                );
            }
        }
    }

    #[test]
    fn silk_internal_fs_hz_matches_literal_version() {
        for config in 0..=u8::MAX {
            let literal = match config >> 2 {
                0 => Some(8_000),
                1 => Some(12_000),
                2 | 3 => Some(16_000),
                _ => None,
            };
            assert_eq!(silk_internal_fs_hz(config).ok(), literal, "config {config}");
        }
    }

    #[test]
    fn unpacks_lbrr_bitfield() {
        assert_eq!(unpack_lbrr_symbol(0b001, 3), [true, false, false]);
        assert_eq!(unpack_lbrr_symbol(0b011, 3), [true, true, false]);
        assert_eq!(unpack_lbrr_symbol(0b101, 3), [true, false, true]);
    }

    #[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
    #[test]
    fn testvector02_pkt0_nlsf_to_lpc_matches_cref() {
        let records = read_opus_demo_records(&vectors_dir().join("testvector02.bit")).unwrap();
        let packet = records[0].packet.clone().unwrap();
        let mut decoder = Decoder::new(48_000, 2).unwrap();
        let mut out = vec![0i16; decoder.max_frame_size_per_channel() * 2];
        let samples = decoder.decode(Some(&packet), &mut out).unwrap();
        assert_eq!(samples, 2880);
        assert_eq!(decoder.final_range(), 0x5037_3c71);
        assert_eq!(
            &decoder.silk().channel_state[0].pred_coef_q12[0][0][..10],
            &TV02_PKT0_LPC_Q12
        );
        assert_eq!(
            &decoder.silk().channel_state[0].pred_coef_q12[0][1][..10],
            &TV02_PKT0_LPC_Q12
        );
    }

    #[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
    #[test]
    fn testvector02_final_range_matches_reference_for_all_packets() {
        let records = read_opus_demo_records(&vectors_dir().join("testvector02.bit")).unwrap();
        let mut decoder = Decoder::new(48_000, 2).unwrap();
        let mut out = vec![0i16; decoder.max_frame_size_per_channel() * 2];
        for (packet_idx, packet) in records.iter().enumerate() {
            let _samples = decoder.decode(packet.packet.as_deref(), &mut out).unwrap();
            assert_eq!(
                decoder.final_range(),
                packet.expected_final_range,
                "packet {packet_idx} final_range"
            );
        }
    }
}

//! Decodificador SILK.
//!
//! Decodifica la cabecera de los paquetes SILK, su información lateral y la síntesis, con una
//! implementación cercana al decodificador de referencia.

use crate::Error;
use crate::entropy::EcDec;
use crate::packet::{DURATIONS_PER_BANDWIDTH, OPUS_RATE_HZ, SILK_FRAME_SAMPLES_48K};
use tables::{
    FS_KHZ_MB, FS_KHZ_NB, FS_KHZ_WB, HZ_PER_KHZ, ICDF_FTB, MAX_NB_SUBFR, NB_SUBFR_10MS,
    NLSF_INTERP_NONE_Q2, SUB_FRAME_LENGTH_MS,
};

mod cng;
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
/// Expansión de ancho de banda de los LPC de la primera trama buena tras una pérdida: 0,97 en
/// Q16 (`BWE_AFTER_LOSS_Q16`, `silk/define.h`).
const BWE_AFTER_LOSS_Q16: i32 = 63_570;
/// Frecuencia interna de SILK para cada ancho de banda de la configuración del TOC: estrecha,
/// media, ancha y, en las híbridas superanchas, también ancha (RFC 6716 §3.1, Tabla 2).
const SILK_INTERNAL_FS_KHZ_BY_BANDWIDTH: [u32; 4] = [FS_KHZ_NB, FS_KHZ_MB, FS_KHZ_WB, FS_KHZ_WB];
const MAX_API_FRAME_SAMPLES_48K: usize = 960 * MAX_INTERNAL_FRAMES;

/// Muestras de PCM interno que caben en un paquete: tres tramas internas de la mayor longitud.
const INTERNAL_PCM_LEN: usize = MAX_INTERNAL_FRAMES * tables::MAX_FRAME_LENGTH;

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

/// Qué se decodifica de una trama SILK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SilkPayload {
    /// La carga principal; las tramas LBRR que lleve se leen y se descartan.
    Main,
    /// Las tramas LBRR (FEC en banda), para recuperar el paquete anterior perdido. Las tramas
    /// internas sin LBRR se ocultan.
    Lbrr,
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

/// Una trama interna de un canal: su posición en el paquete, su bit de actividad de voz, si es
/// una trama LBRR, su codificación condicional y su longitud.
#[derive(Debug, Clone, Copy)]
struct InternalFrame {
    index: usize,
    vad: bool,
    lbrr: bool,
    cond: indices::CondCoding,
    frame_length: usize,
}

/// PCM interno de un paquete por canal, en tramas internas de `frame_length` muestras.
struct InternalPcm<'a> {
    channels: [&'a [i16]; 2],
    packet_channels: usize,
    frame_length: usize,
    /// Paquete mono tras uno estéreo a la misma frecuencia interna (ver
    /// [`SilkDecoder::collapses_stereo`]).
    collapses_stereo: bool,
}

/// Estado persistente de un canal codificado de SILK: índices, síntesis, remuestreo y
/// coeficientes LPC de las tramas internas del último paquete.
#[derive(Debug, Clone)]
struct SilkChannel {
    /// Estado de la lectura de índices (frecuencia interna, subtramas y valores previos).
    indices: indices::ChannelState,
    /// Estado de la síntesis y de la ocultación de pérdidas.
    core: decode_core::SilkChannelState,
    /// Remuestreador a la frecuencia de la API; se crea con la primera trama del canal.
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
    /// se reinicia solo la historia de la síntesis, como `silk_decoder_set_fs` de libopus: la
    /// ganancia anterior y la excitación se conservan, y la ocultación y el ruido de confort se
    /// reinician solos al ver la frecuencia nueva.
    fn set_internal_rate(&mut self, fs_khz: u32, nb_subfr: usize) {
        let prev_fs_khz = self.indices.fs_khz;
        if prev_fs_khz != 0 && prev_fs_khz != fs_khz {
            self.reset_synthesis_history();
        }
        indices::configure_channel_state(&mut self.indices, fs_khz, nb_subfr);
    }

    /// Devuelve el canal a su estado inicial, como `silk_init_decoder` de libopus.
    fn restart(&mut self) {
        *self = Self::default();
    }

    /// Reinicia la historia de la síntesis del canal (ver
    /// [`decode_core::SilkChannelState::reset_synthesis_history`]) y hace que la siguiente trama
    /// no interpole las NLSF. Es lo que hace `silk_Decode` de libopus con el canal lateral al
    /// volver a codificarse tras una trama solo de medio.
    fn reset_synthesis_history(&mut self) {
        self.core.reset_synthesis_history();
        self.first_frame_after_reset = true;
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

    /// Decodifica la trama interna `frame` de este canal en `pcm` como `silk_decode_frame` de
    /// libopus: índices, coeficientes LPC (expandidos si es la primera tras una pérdida), pulsos y
    /// síntesis; después, la actualización de la ocultación, el ruido de confort y el fundido con
    /// la ocultación anterior.
    ///
    /// # Errores
    ///
    /// `BadPacket` si los índices o los pulsos no se pueden decodificar.
    fn decode_internal_frame(
        &mut self,
        dec: &mut EcDec<'_>,
        frame: InternalFrame,
        pcm: &mut [i16],
    ) -> Result<(), Error> {
        let side =
            indices::decode_indices(&mut self.indices, dec, frame.vad, frame.lbrr, frame.cond)?;
        let fs_khz = self.indices.fs_khz;
        let after_loss = self.core.plc.loss_cnt() > 0;
        let pred_coef_q12 = &mut self.pred_coef_q12[frame.index];
        update_lpc_from_nlsf(
            fs_khz,
            &side.nlsf,
            &mut self.prev_nlsf_q15,
            pred_coef_q12,
            &mut self.first_frame_after_reset,
        );
        let lpc_order = tables::nlsf_codebook(fs_khz).order;
        if after_loss {
            for half in pred_coef_q12.iter_mut() {
                lpc::bwexpander(half, lpc_order, BWE_AFTER_LOSS_Q16);
            }
        }
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
            lpc_order,
        };
        let control = decode_core::decode_core(&params, pred_coef_q12, pcm, &mut self.core);
        let nb_subfr = params.nb_subfr;
        let subfr_length = frame.frame_length / nb_subfr;
        self.core.plc.update(&plc::DecodedFrame {
            signal_type: params.signal_type,
            control: &control,
            pred_coef_q12: &self.pred_coef_q12[frame.index][1][..lpc_order],
            nb_subfr,
            subfr_length,
            fs_khz,
        });
        self.comfort_noise_and_glue(&control.gains_q16[..nb_subfr], pcm);
        self.core.lag_prev = control.pitch_l[nb_subfr - 1];
        Ok(())
    }

    /// Oculta una trama interna perdida en `pcm` como `silk_decode_frame` de libopus con
    /// pérdida: la ocultación, la salida reciente, el ruido de confort y la energía para el
    /// fundido. La trama dura lo que la última decodificada.
    fn conceal(&mut self, pcm: &mut [i16]) {
        let fs_khz = self.indices.fs_khz;
        let core = &mut self.core;
        let input = plc::ConcealInput {
            exc_q14: &core.exc_q14,
            out_buf: &core.out_buf,
            prev_signal_type: core.prev_signal_type,
            first_frame_after_reset: self.first_frame_after_reset,
            fs_khz,
            lpc_order: tables::nlsf_codebook(fs_khz).order,
            nb_subfr: self.indices.nb_subfr,
            subfr_length: SUB_FRAME_LENGTH_MS * fs_khz as usize,
        };
        let lag = core.plc.conceal(&input, &mut core.s_lpc_q14_buf, pcm);
        core.record_concealed_frame(pcm, fs_khz);
        self.comfort_noise_and_glue(&[], pcm);
        self.core.lag_prev = lag;
    }

    /// Ruido de confort y fundido con la ocultación de la trama `pcm`, buena o perdida
    /// (`silk_CNG` y `silk_PLC_glue_frames`). `gains_q16` son las ganancias de la trama buena.
    fn comfort_noise_and_glue(&mut self, gains_q16: &[i32], pcm: &mut [i16]) {
        let fs_khz = self.indices.fs_khz;
        let core = &mut self.core;
        let (plc_rand_scale_q14, plc_prev_gain_q16) = core.plc.noise_gain_inputs();
        let input = cng::CngInput {
            fs_khz,
            lpc_order: tables::nlsf_codebook(fs_khz).order,
            loss_cnt: core.plc.loss_cnt(),
            prev_signal_type: core.prev_signal_type,
            prev_nlsf_q15: &self.prev_nlsf_q15,
            gains_q16,
            nb_subfr: self.indices.nb_subfr,
            subfr_length: SUB_FRAME_LENGTH_MS * fs_khz as usize,
            exc_q14: &core.exc_q14,
            plc_rand_scale_q14,
            plc_prev_gain_q16,
        };
        core.cng.apply(&input, pcm);
        core.plc.glue_frames(pcm);
    }
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

    /// Muestras de cada trama interna ocultada: las de la última trama decodificada del canal 0,
    /// o `None` si todavía no se ha decodificado ninguna.
    fn concealment_frame_samples(&self) -> Option<usize> {
        let indices = &self.channel_state[0].indices;
        let samples = indices.nb_subfr * SUB_FRAME_LENGTH_MS * indices.fs_khz as usize;
        (samples != 0).then_some(samples)
    }

    /// Muestras por canal de salida que da una trama interna de `internal_samples` muestras.
    fn output_len(&self, internal_samples: usize) -> usize {
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
            let internal_pcm = self.conceal_internal_frame(packet_channels, frame_samples);
            let frame_write = frame_output_samples.min(samples_per_channel - written_total);
            let pcm = InternalPcm {
                channels: [
                    &internal_pcm[0][..frame_samples],
                    &internal_pcm[1][..frame_samples],
                ],
                packet_channels,
                frame_length: frame_samples,
                collapses_stereo: false,
            };
            self.emit(&pcm, &mut mono_history, out, written_total, frame_write);
            written_total += frame_write;
        }
        Ok(samples_per_channel)
    }

    /// Oculta una trama interna de `frame_samples` muestras en cada canal del último paquete,
    /// como `silk_Decode` de libopus con pérdida: el lateral se oculta si la trama anterior no
    /// era solo de medio (y si lo era, se reinicia y queda en silencio); en estéreo, medio y
    /// lateral pasan a izquierdo y derecho con la predicción anterior; y al final ningún canal
    /// limita la bajada de la ganancia de la trama siguiente.
    fn conceal_internal_frame(
        &mut self,
        packet_channels: usize,
        frame_samples: usize,
    ) -> [[i16; tables::MAX_FRAME_LENGTH]; 2] {
        let mut pcm = [[0i16; tables::MAX_FRAME_LENGTH]; 2];
        let stereo = packet_channels == 2;
        if stereo && self.prev_decode_only_middle {
            self.channel_state[1].reset_synthesis_history();
        }
        let has_side = !self.prev_decode_only_middle;
        for (ch, (channel, frame_pcm)) in self
            .channel_state
            .iter_mut()
            .zip(pcm.iter_mut())
            .take(packet_channels)
            .enumerate()
        {
            if ch == 0 || has_side {
                channel.conceal(&mut frame_pcm[..frame_samples]);
            }
        }
        let pred_q13 = self.stereo_state.pred_prev_q13.map(i32::from);
        let [left, right] = &mut pcm;
        self.join_channels(
            &mut left[..frame_samples],
            &mut right[..frame_samples],
            packet_channels,
            pred_q13,
        );
        for channel in self.channel_state.iter_mut().take(packet_channels) {
            channel.core.forget_last_gain_after_loss();
        }
        pcm
    }

    /// Termina una trama interna de los canales del paquete: con un paquete y una salida estéreo,
    /// pasa medio (`left`) y lateral (`right`) a izquierdo y derecho con la predicción
    /// `pred_q13`; si no, guarda las dos últimas muestras del medio para retrasarlo una muestra al
    /// remuestrear, y la salida es el medio, como en `silk_Decode` de libopus (`silk/dec_API.c`),
    /// que solo convierte si `nChannelsAPI` y `nChannelsInternal` son 2.
    fn join_channels(
        &mut self,
        left: &mut [i16],
        right: &mut [i16],
        packet_channels: usize,
        pred_q13: [i32; 2],
    ) {
        if self.converts_to_left_right(packet_channels) {
            stereo::ms_to_lr(
                &mut self.stereo_state,
                left,
                right,
                pred_q13,
                self.channel_state[0].indices.fs_khz,
            );
        } else {
            self.stereo_state
                .s_mid
                .copy_from_slice(&left[left.len() - 2..]);
        }
    }

    /// Si el PCM interno de un paquete de `packet_channels` canales se convierte de medio y
    /// lateral a izquierdo y derecho: solo con paquete y salida estéreo.
    fn converts_to_left_right(&self, packet_channels: usize) -> bool {
        packet_channels == 2 && self.channels == 2
    }

    /// Escribe en `out`, desde la muestra por canal `first`, `count` muestras por canal del PCM
    /// interno `pcm`, remuestreado a la frecuencia de salida. Si no hay conversión a izquierdo y
    /// derecho, sale el medio, y su remuestreo usa y actualiza `mono_history`; con salida estéreo,
    /// el derecho lo copia, salvo en la primera trama interna de un paquete que colapsa el
    /// estéreo (ver [`Self::collapses_stereo`]).
    fn emit(
        &mut self,
        pcm: &InternalPcm<'_>,
        mono_history: &mut [i16; 2],
        out: &mut [i16],
        first: usize,
        count: usize,
    ) {
        let left_right = self.converts_to_left_right(pcm.packet_channels);
        let stereo_out = self.channels == 2;
        let mut resampled = [[0i16; MAX_API_FRAME_SAMPLES_48K]; 2];
        let mid_history = *mono_history;
        for (ch, channel) in self
            .channel_state
            .iter_mut()
            .enumerate()
            .take(if left_right { 2 } else { 1 })
        {
            if let Some(resampler) = channel.resampler.as_mut() {
                let history = (!left_right).then_some(&mut *mono_history);
                resample_frames(
                    resampler,
                    pcm.channels[ch],
                    pcm.frame_length,
                    &mut resampled[ch],
                    history,
                );
            }
        }
        let collapsed = stereo_out
            && pcm.collapses_stereo
            && self.resample_collapsed_right(pcm, mid_history, &mut resampled, count);
        write_interleaved(
            out,
            first,
            &resampled[0][..count],
            (left_right || collapsed).then_some(&resampled[1][..count]),
            stereo_out,
        );
    }

    /// Canal derecho de un paquete mono que colapsa el estéreo, en `resampled[1]`: la primera
    /// trama interna del medio, retrasada desde `mid_history` como la del izquierdo, pasa por el
    /// remuestreador del derecho, que conserva el estado del tramo estéreo; el resto, hasta
    /// `count` muestras, copia el izquierdo de `resampled[0]`. Es lo que hace `silk_Decode` de
    /// libopus con `stereo_to_mono`, que solo vale en la primera llamada del paquete. Devuelve
    /// `false`, sin escribir, si el derecho no tiene remuestreador.
    fn resample_collapsed_right(
        &mut self,
        pcm: &InternalPcm<'_>,
        mut mid_history: [i16; 2],
        resampled: &mut [[i16; MAX_API_FRAME_SAMPLES_48K]; 2],
        count: usize,
    ) -> bool {
        let Some(resampler) = self.channel_state[1].resampler.as_mut() else {
            return false;
        };
        let [left, right] = resampled;
        let first_frame = &pcm.channels[0][..pcm.frame_length];
        let written = resample_delayed(resampler, first_frame, right, &mut mid_history).min(count);
        right[written..count].copy_from_slice(&left[written..count]);
        true
    }

    /// Si un paquete de `packet_channels` canales a la frecuencia interna `internal_fs_hz` colapsa
    /// el estéreo: es mono y sigue a uno estéreo a la misma frecuencia interna, como
    /// `stereo_to_mono` en `silk_Decode` de libopus (`silk/dec_API.c`). Se decide antes de fijar
    /// la frecuencia del paquete; tras un reinicio no hay frecuencia anterior y no colapsa.
    fn collapses_stereo(&self, packet_channels: usize, internal_fs_hz: u32) -> bool {
        packet_channels == 1
            && self.prev_packet_channels == 2
            && self.channel_state[0].indices.fs_khz * HZ_PER_KHZ == internal_fs_hz
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
        let samples_per_channel = self.decode_payload(dec, request, SilkPayload::Main, out)?;
        self.finish(dec)?;
        Ok(samples_per_channel)
    }

    /// Cierra una trama: guarda el estado final del decodificador de rango. En las tramas solo
    /// SILK el llamador lo llama tras leer la cola de redundancia.
    ///
    /// # Errores
    ///
    /// `BadPacket` si la lectura se salió de la trama.
    pub(crate) fn finish(&mut self, dec: &EcDec<'_>) -> Result<(), Error> {
        self.final_range = dec.final_range();
        if dec.is_error() {
            return Err(Error::BadPacket);
        }
        Ok(())
    }

    /// Decodifica las tramas internas del paquete, las principales o las LBRR según `payload`, y
    /// escribe su PCM en `out`; devuelve las muestras por canal de salida. No cierra la trama: el
    /// llamador lee lo que quede (la cola de redundancia de una trama solo SILK) y llama a
    /// [`Self::finish`].
    ///
    /// # Errores
    ///
    /// Los de [`Self::decode_frame_with_ec`] salvo el de lectura fuera de la trama.
    pub(crate) fn decode_payload(
        &mut self,
        dec: &mut EcDec<'_>,
        request: SilkFrameRequest,
        payload: SilkPayload,
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

        let collapses_stereo = self.collapses_stereo(packet_channels, internal_fs_hz);
        self.prepare_channels(packet_channels, internal_fs_hz, nb_subfr)?;
        let header = parse_header(dec, packet_channels, internal_frames)?;
        let layout = FrameLayout {
            packet_channels,
            frame_length,
        };
        let mut internal_pcm = [[0i16; INTERNAL_PCM_LEN]; 2];
        match payload {
            SilkPayload::Main => {
                consume_lbrr_payload(
                    dec,
                    &header,
                    packet_channels,
                    internal_frames,
                    &mut self.channel_state,
                    frame_length,
                )?;
                for i in 0..internal_frames {
                    self.decode_internal_frame(dec, &header, layout, i, &mut internal_pcm)?;
                }
            }
            SilkPayload::Lbrr => {
                for i in 0..internal_frames {
                    self.decode_lbrr_internal_frame(dec, &header, layout, i, &mut internal_pcm)?;
                }
            }
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
            collapses_stereo,
        };
        self.emit(&pcm, &mut mono_history, out, 0, samples_per_channel);
        self.prev_packet_channels = packet_channels as u8;
        Ok(samples_per_channel)
    }

    /// Prepara los canales del paquete para la frecuencia interna `internal_fs_hz` y sus
    /// remuestreadores. Al pasar de un paquete mono a uno estéreo, el canal derecho empieza
    /// de cero, como en `silk_Decode` de libopus (`silk_init_decoder`), sea cual sea la salida;
    /// con salida estéreo, además, se borra la predicción estéreo y el canal derecho parte del
    /// remuestreador del izquierdo.
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
        if packet_channels == 2 && self.prev_packet_channels == 1 {
            self.channel_state[1].restart();
        }
        for channel in self.channel_state.iter_mut().take(packet_channels) {
            channel.set_internal_rate(fs_khz, nb_subfr);
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
    /// estéreo, cada canal (con su ocultación y su ruido de confort) y la conversión de medio y
    /// lateral a izquierdo y derecho.
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
            if !decode_only_middle && self.prev_decode_only_middle {
                self.channel_state[1].reset_synthesis_history();
            }
        }

        let channels = self
            .channel_state
            .iter_mut()
            .zip(internal_pcm.iter_mut())
            .zip(&header.channels)
            .take(packet_channels);
        for (ch, ((channel, pcm), channel_header)) in channels.enumerate() {
            if ch == 1 && decode_only_middle {
                continue;
            }
            let frame = InternalFrame {
                index: i,
                vad: channel_header.vad_flags[i],
                lbrr: false,
                cond: main_cond_coding(i, ch, self.prev_decode_only_middle),
                frame_length,
            };
            channel.decode_internal_frame(dec, frame, &mut pcm[range.clone()])?;
        }

        let [left, right] = &mut *internal_pcm;
        self.join_channels(
            &mut left[range.clone()],
            &mut right[range],
            packet_channels,
            pred_q13,
        );
        self.prev_decode_only_middle = decode_only_middle;
        Ok(())
    }

    /// Decodifica la trama LBRR de la trama interna `frame.index` en `internal_pcm`, como
    /// `silk_Decode` de libopus con `lostFlag == FLAG_DECODE_LBRR`: la predicción estéreo y el
    /// indicador de solo medio son los de la LBRR si el canal medio la trae (si no, se repite la
    /// predicción anterior); el lateral se decodifica si la trama anterior no era solo de medio o
    /// si trae LBRR; y la trama de un canal sin LBRR se oculta.
    ///
    /// # Errores
    ///
    /// `BadPacket` si los índices o los pulsos de una trama LBRR no se pueden decodificar.
    fn decode_lbrr_internal_frame(
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
        let lbrr = |ch: usize| header.channels[ch].lbrr_flags[i];
        let mut pred_q13 = self.stereo_state.pred_prev_q13.map(i32::from);
        let mut decode_only_middle = false;
        if packet_channels == 2 {
            if lbrr(0) {
                pred_q13 = stereo::decode_stereo_pred(dec);
                decode_only_middle = !lbrr(1) && stereo::decode_mid_only(dec);
            }
            if !decode_only_middle && self.prev_decode_only_middle {
                self.channel_state[1].reset_synthesis_history();
            }
        }
        let has_side = lbrr_has_side(
            self.prev_decode_only_middle,
            packet_channels == 2 && lbrr(1),
        );

        let channels = self
            .channel_state
            .iter_mut()
            .zip(internal_pcm.iter_mut())
            .zip(&header.channels)
            .take(packet_channels);
        for (ch, ((channel, pcm), channel_header)) in channels.enumerate() {
            if ch == 1 && !has_side {
                continue;
            }
            let pcm = &mut pcm[range.clone()];
            if !channel_header.lbrr_flags[i] {
                channel.conceal(pcm);
                continue;
            }
            let lbrr_frame = InternalFrame {
                index: i,
                vad: channel_header.vad_flags[i],
                lbrr: true,
                cond: lbrr_cond_coding(i, &channel_header.lbrr_flags),
                frame_length,
            };
            channel.decode_internal_frame(dec, lbrr_frame, pcm)?;
        }

        let [left, right] = &mut *internal_pcm;
        self.join_channels(
            &mut left[range.clone()],
            &mut right[range],
            packet_channels,
            pred_q13,
        );
        self.prev_decode_only_middle = decode_only_middle;
        Ok(())
    }
}

/// Codificación condicional de la trama LBRR interna `i` de un canal con indicadores
/// `lbrr_flags`, como `silk_Decode` de libopus con `FLAG_DECODE_LBRR`: la primera, independiente;
/// las demás, condicionadas si la trama anterior del canal también trae LBRR.
fn lbrr_cond_coding(i: usize, lbrr_flags: &[bool; MAX_INTERNAL_FRAMES]) -> indices::CondCoding {
    if i > 0 && lbrr_flags[i - 1] {
        indices::CondCoding::Conditionally
    } else {
        indices::CondCoding::Independently
    }
}

/// Si se decodifica el canal lateral de una trama LBRR, como `has_side` en `silk_Decode` de
/// libopus: cuando la trama anterior no era solo de medio o cuando el lateral trae LBRR.
fn lbrr_has_side(prev_decode_only_middle: bool, side_has_lbrr: bool) -> bool {
    !prev_decode_only_middle || side_has_lbrr
}

/// Codificación condicional de la trama interna `i` del canal `ch`, en el orden de `silk_Decode`
/// de libopus: la primera trama del paquete nunca se condiciona; después, el canal lateral que
/// sigue a una trama solo de medio no escala el LTP, y las demás se condicionan a la anterior.
fn main_cond_coding(i: usize, ch: usize, prev_decode_only_middle: bool) -> indices::CondCoding {
    if i == 0 {
        indices::CondCoding::Independently
    } else if ch > 0 && prev_decode_only_middle {
        indices::CondCoding::IndependentlyNoLtpScaling
    } else {
        indices::CondCoding::Conditionally
    }
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
    /// Frecuencia de salida de las pruebas que preparan canales (remuestreo a 48 kHz).
    const OUTPUT_48K_HZ: u32 = 48_000;
    /// Frecuencia interna de banda ancha y subtramas de una trama de 20 ms.
    const WIDEBAND_HZ: u32 = 16_000;
    const SUBFRAMES_20_MS: usize = 4;
    /// Ganancia anterior cualquiera distinta de la inicial: solo la borra un reinicio completo.
    const STALE_PREV_GAIN_Q16: i32 = 1234;

    /// Al pasar de un paquete mono a uno estéreo, el canal derecho empieza de cero también con
    /// salida mono: `silk_Decode` de libopus llama a `silk_init_decoder(&channel_state[1])` sin
    /// mirar los canales de salida. Con salida estéreo lo vigila además el oráculo `empalmes`
    /// (`tests/referencia_libopus.rs`); con salida mono, solo esta prueba.
    #[test]
    fn mono_to_stereo_restarts_right_channel_with_any_output() {
        for channels in [1, 2] {
            let mut decoder = SilkDecoder::new(OUTPUT_48K_HZ, channels);
            for channel in &mut decoder.channel_state {
                channel.set_internal_rate(WIDEBAND_HZ / HZ_PER_KHZ, SUBFRAMES_20_MS);
            }
            let stale = &mut decoder.channel_state[1];
            stale.core.prev_gain_q16 = STALE_PREV_GAIN_Q16;
            stale.first_frame_after_reset = false;

            decoder
                .prepare_channels(2, WIDEBAND_HZ, SUBFRAMES_20_MS)
                .unwrap();

            let right = &decoder.channel_state[1];
            let initial = SilkChannel::default();
            assert_eq!(
                right.core.prev_gain_q16, initial.core.prev_gain_q16,
                "{channels} canales"
            );
            assert!(right.first_frame_after_reset, "{channels} canales");
        }
    }

    /// Un cambio de frecuencia interna reinicia solo la historia de la síntesis, como
    /// `silk_decoder_set_fs` de libopus: la ganancia anterior y la excitación se conservan.
    #[test]
    fn internal_rate_change_keeps_gain_and_excitation() {
        const NARROWBAND_KHZ: u32 = 8;
        const STALE_EXCITATION_Q14: i32 = -77;
        let mut channel = SilkChannel::default();
        channel.set_internal_rate(NARROWBAND_KHZ, SUBFRAMES_20_MS);
        channel.core.prev_gain_q16 = STALE_PREV_GAIN_Q16;
        channel.core.exc_q14.fill(STALE_EXCITATION_Q14);
        channel.first_frame_after_reset = false;

        channel.set_internal_rate(WIDEBAND_HZ / HZ_PER_KHZ, SUBFRAMES_20_MS);

        assert_eq!(channel.core.prev_gain_q16, STALE_PREV_GAIN_Q16);
        assert!(
            channel
                .core
                .exc_q14
                .iter()
                .all(|&e| e == STALE_EXCITATION_Q14)
        );
        assert!(channel.first_frame_after_reset);
    }

    /// El reinicio del canal lateral tras una trama solo de medio hace que la trama siguiente no
    /// interpole las NLSF, como `first_frame_after_reset = 1` en `silk_Decode`. Ni la red de
    /// equivalencia ni los vectores lo distinguen (medido: quitarlo no cambia ninguna salida).
    #[test]
    fn side_reset_after_mid_only_skips_nlsf_interpolation() {
        let mut channel = SilkChannel {
            first_frame_after_reset: false,
            ..SilkChannel::default()
        };
        channel.reset_synthesis_history();
        assert!(channel.first_frame_after_reset);
    }

    /// Codificación condicional que elige `silk_Decode` de libopus (sin LBRR), como oráculo:
    /// la primera trama del paquete siempre independiente; después, el canal lateral que sigue a
    /// una trama solo de medio sin escala LTP; el resto, condicionada.
    fn libopus_cond_coding(
        i: usize,
        ch: usize,
        prev_decode_only_middle: bool,
    ) -> indices::CondCoding {
        if i == 0 {
            indices::CondCoding::Independently
        } else if ch > 0 && prev_decode_only_middle {
            indices::CondCoding::IndependentlyNoLtpScaling
        } else {
            indices::CondCoding::Conditionally
        }
    }

    #[test]
    fn main_cond_coding_matches_libopus() {
        for i in 0..MAX_INTERNAL_FRAMES {
            for ch in 0..2 {
                for prev in [false, true] {
                    assert_eq!(
                        main_cond_coding(i, ch, prev),
                        libopus_cond_coding(i, ch, prev),
                        "trama {i}, canal {ch}, anterior solo medio: {prev}"
                    );
                }
            }
        }
    }

    /// Codificación condicional y `has_side` de `silk_Decode` (`silk/dec_API.c`, v1.6.1) con
    /// `FLAG_DECODE_LBRR`, con sus mismas condiciones, como oráculo: `FrameIndex <= 0` da
    /// independiente; si no, condicionada si la LBRR anterior del canal existe.
    fn libopus_lbrr_cond_coding(frame_index: usize, lbrr_flags: [bool; 3]) -> indices::CondCoding {
        if frame_index == 0 {
            indices::CondCoding::Independently
        } else if lbrr_flags[frame_index - 1] {
            indices::CondCoding::Conditionally
        } else {
            indices::CondCoding::Independently
        }
    }

    #[test]
    fn lbrr_cond_coding_matches_libopus() {
        for bits in 0u8..8 {
            let flags = [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0];
            for i in 0..MAX_INTERNAL_FRAMES {
                assert_eq!(
                    lbrr_cond_coding(i, &flags),
                    libopus_lbrr_cond_coding(i, flags),
                    "trama {i}, indicadores LBRR {flags:?}"
                );
            }
        }
    }

    #[test]
    fn lbrr_has_side_matches_libopus() {
        // `has_side = !prev_decode_only_middle || (estéreo && LBRR lateral)`.
        for prev in [false, true] {
            for side_lbrr in [false, true] {
                assert_eq!(lbrr_has_side(prev, side_lbrr), !prev || side_lbrr);
            }
        }
    }

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

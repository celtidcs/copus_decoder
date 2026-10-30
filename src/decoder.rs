//! Decodificador de un flujo Opus: elige el modo de cada paquete (SILK, híbrido o CELT), encadena
//! las capas, decodifica la redundancia de las transiciones y oculta los paquetes perdidos. La API
//! pública (`OpusDecoder`) vive en `lib.rs` y delega aquí.

use crate::celt::CeltOutput;
use crate::entropy::EcDec;
use crate::packet::{self, BITS_PER_BYTE, LAST_HYBRID_CONFIG, LAST_SILK_ONLY_CONFIG, OPUS_RATE_HZ};
use crate::pcm::{SUPPORTED_RATES_HZ, saturate_to_i16};
use crate::silk::SilkPayload;
use crate::{Error, celt, silk};

/// Atenuación por paquete perdido de la ocultación sin CELT, y número máximo de pérdidas que
/// acumula. Son valores de la implementación original; no se han cotejado con libopus.
const CONCEAL_FADE_PER_LOSS: f32 = 0.9;
const CONCEAL_MAX_FADE_STEPS: u32 = 10;
/// Tramas de 5 ms y de 2,5 ms por segundo. En libopus, `F5` es la duración de la trama CELT
/// redundante de una transición y `F2_5`, la del fundido con ella (`opus_decode_frame`).
const FIVE_MS_PER_SECOND: usize = 200;
const TWO_AND_HALF_MS_PER_SECOND: usize = 400;
/// Tramas de 10 y 20 ms por segundo (`F10` y `F20` de `opus_decode_frame`).
const TEN_MS_PER_SECOND: usize = 100;
const TWENTY_MS_PER_SECOND: usize = 50;
/// La trama redundante de 5 ms, a 48 kHz.
const REDUNDANT_FRAME_SAMPLES_48K: usize = OPUS_RATE_HZ / FIVE_MS_PER_SECOND;
/// Trama CELT de silencio con la que libopus extingue CELT al pasar de híbrido a solo SILK, tal
/// cual la escribe `opus_decode_frame` (`silence[2] = {0xFF, 0xFF}`).
const CELT_SILENCE_FRAME: [u8; 2] = [0xFF, 0xFF];
/// Duración de esa trama de silencio: 2,5 ms a 48 kHz (`F2_5` en `opus_decode_frame`).
const CELT_FADE_OUT_SAMPLES_48K: usize = OPUS_RATE_HZ / TWO_AND_HALF_MS_PER_SECOND;
/// Bits que deben quedar en la trama para leer la señal de redundancia: 17, y 20 más en híbrido
/// (`ec_tell(&dec)+17+20*(mode == MODE_HYBRID) <= 8*len` en `opus_decode_frame`).
const REDUNDANCY_MIN_BITS: i32 = 17;
const REDUNDANCY_HYBRID_EXTRA_BITS: i32 = 20;
/// La señal de redundancia híbrida tiene probabilidad 1/2^12 (`ec_dec_bit_logp(&dec, 12)`).
const REDUNDANCY_FLAG_LOGP: u32 = 12;
/// La longitud de la redundancia híbrida es uniforme en `0..256`, más 2 bytes
/// (`ec_dec_uint(&dec, 256)+2`).
const REDUNDANCY_SIZE_RANGE: u32 = 256;
const REDUNDANCY_MIN_BYTES: usize = 2;
/// Tramas de este tamaño o menos se ocultan como pérdidas, como en `opus_decode_frame` de libopus
/// ("Payloads of 1 (2 including ToC) or 0 trigger the PLC/DTX").
const MAX_DTX_FRAME_BYTES: usize = 1;
/// Primera banda CELT fuera del modo solo CELT: por debajo codifica SILK (`start_band = 17` en
/// `opus_decode_frame`).
const HYBRID_CELT_START_BAND: usize = 17;

#[derive(Debug, Clone)]
pub(crate) struct Decoder {
    fs_hz: u32,
    channels: u8,
    celt: celt::CeltDecoder,
    silk: silk::SilkDecoder,
    last_packet_split_count: usize,
    last_final_range: u32,
    last_had_redundancy: bool,
    last_celt_to_silk: bool,
    prev_mode: Option<OpusMode>,
    prev_redundancy: bool,
    loss_count: u32,
    last_packet_duration: usize,
    /// Duración de cada trama del último paquete, en muestras por canal (`st->frame_size`): una
    /// llamada a la ocultación no pasa de ella.
    frame_size: usize,
    last_output: Vec<i16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpusMode {
    SilkOnly,
    Hybrid,
    CeltOnly,
}

impl Decoder {
    pub(crate) const MAX_FRAME_SIZE_48K: usize = packet::MAX_DURATION_SAMPLES_48K;

    pub(crate) fn new(fs_hz: u32, channels: u8) -> Result<Self, Error> {
        if !SUPPORTED_RATES_HZ.contains(&fs_hz) {
            return Err(Error::InvalidSampleRate(fs_hz));
        }
        if !matches!(channels, 1 | 2) {
            return Err(Error::InvalidChannels(channels));
        }
        Ok(Self {
            fs_hz,
            channels,
            celt: celt::CeltDecoder::new(fs_hz, channels),
            silk: silk::SilkDecoder::new(fs_hz, channels),
            last_packet_split_count: 0,
            last_final_range: 0,
            last_had_redundancy: false,
            last_celt_to_silk: false,
            prev_mode: None,
            prev_redundancy: false,
            loss_count: 0,
            last_packet_duration: 0,
            frame_size: fs_hz as usize / TWO_AND_HALF_MS_PER_SECOND,
            last_output: Vec::new(),
        })
    }

    /// Estado interno de SILK, para que las pruebas unitarias lo comparen con libopus.
    #[cfg(test)]
    pub(crate) fn silk(&self) -> &silk::SilkDecoder {
        &self.silk
    }

    /// Muestras por canal del último paquete decodificado sin error (0 tras crear o reiniciar);
    /// la ocultación de una pérdida dura lo mismo y no lo cambia.
    pub(crate) fn last_packet_duration(&self) -> usize {
        self.last_packet_duration
    }

    pub(crate) fn fs_hz(&self) -> u32 {
        self.fs_hz
    }

    pub(crate) fn channels(&self) -> u8 {
        self.channels
    }

    pub(crate) fn max_frame_size_per_channel(&self) -> usize {
        // 120 ms a la frecuencia de salida; `new` solo admite frecuencias que dividen 48 kHz.
        Self::MAX_FRAME_SIZE_48K * self.fs_hz as usize / OPUS_RATE_HZ
    }

    /// Muestras por canal de 5 ms a la frecuencia de salida (`F5` de libopus).
    fn samples_5ms(&self) -> usize {
        self.fs_hz as usize / FIVE_MS_PER_SECOND
    }

    /// Muestras por canal de 2,5 ms a la frecuencia de salida (`F2_5` de libopus).
    fn samples_2_5ms(&self) -> usize {
        self.fs_hz as usize / TWO_AND_HALF_MS_PER_SECOND
    }

    /// Oculta una pérdida sin estado de SILK ni de CELT que imitar repitiendo la última salida,
    /// atenuada según las pérdidas seguidas `loss_count`. Sin salida previa suficiente, escribe
    /// silencio.
    fn conceal_with_fade(&self, frame_size: usize, loss_count: u32, out: &mut [i16]) {
        let channels = self.channels as usize;
        let needed = frame_size * channels;
        if self.last_output.len() < needed {
            out[..needed].fill(0);
            return;
        }

        let fade = CONCEAL_FADE_PER_LOSS.powi((loss_count.min(CONCEAL_MAX_FADE_STEPS) + 1) as i32);
        for (dst, src) in out[..needed]
            .iter_mut()
            .zip(self.last_output[..needed].iter())
        {
            *dst = saturate_to_i16(f32::from(*src) * fade);
        }
    }

    /// Guarda una copia de la última salida, de `samples_per_channel` muestras por canal, para la
    /// ocultación por atenuación.
    fn store_last_output(&mut self, out: &[i16], samples_per_channel: usize) {
        let written = samples_per_channel * self.channels as usize;
        self.last_output.clear();
        self.last_output.extend_from_slice(&out[..written]);
    }

    /// Oculta un paquete perdido con la ocultación del modo del paquete anterior (CELT o SILK)
    /// o, sin modo anterior, por atenuación. Dura lo que el último paquete decodificado; si no lo
    /// hay, devuelve 0 muestras.
    ///
    /// # Errores
    ///
    /// `OutputTooSmall` si la ocultación no cabe en `out`.
    fn decode_lost_packet(&mut self, out: &mut [i16]) -> Result<usize, Error> {
        self.conceal_samples(self.last_packet_duration, out)
    }

    /// Oculta `samples_per_channel` muestras por canal perdidas, como [`Self::decode_lost_packet`]
    /// pero con la duración dada; con 0, no escribe nada.
    ///
    /// # Errores
    ///
    /// `OutputTooSmall` si la ocultación no cabe en `out`.
    fn conceal_samples(
        &mut self,
        samples_per_channel: usize,
        out: &mut [i16],
    ) -> Result<usize, Error> {
        if samples_per_channel == 0 {
            self.last_final_range = 0;
            return Ok(0);
        }

        let needed = samples_per_channel * self.channels as usize;
        if out.len() < needed {
            return Err(Error::OutputTooSmall {
                needed,
                got: out.len(),
            });
        }

        out[..needed].fill(0);
        // Como `opus_decode_frame` con `data == NULL`: se oculta con el modo de la última trama,
        // o con CELT si traía redundancia CELT, y ese pasa a ser el modo anterior.
        let mode = if self.prev_redundancy {
            Some(OpusMode::CeltOnly)
        } else {
            self.prev_mode
        };
        match mode {
            Some(mode) => {
                let channels = self.channels as usize;
                let mut written = 0usize;
                while written < samples_per_channel {
                    let remaining = (samples_per_channel - written).min(self.frame_size);
                    let chunk = concealment_chunk(remaining, mode, self.fs_hz);
                    let out_chunk = &mut out[written * channels..(written + chunk) * channels];
                    self.conceal_chunk(mode, chunk, out_chunk)?;
                    written += chunk;
                }
            }
            None => self.conceal_with_fade(samples_per_channel, self.loss_count, out),
        }

        self.prev_mode = mode;
        self.loss_count = self.loss_count.saturating_add(1);
        self.last_packet_split_count = 0;
        self.last_final_range = 0;
        self.last_had_redundancy = false;
        self.last_celt_to_silk = false;
        self.prev_redundancy = false;
        self.store_last_output(out, samples_per_channel);
        Ok(samples_per_channel)
    }

    /// Oculta un trozo de `samples_per_channel` muestras por canal con el modo `mode`, como una
    /// llamada a `opus_decode_frame` de libopus con `data == NULL`: SILK, CELT desde la banda 0
    /// o, en híbrido, SILK y encima el ruido de CELT desde la banda 17.
    ///
    /// # Errores
    ///
    /// Los de la ocultación de SILK y de CELT.
    fn conceal_chunk(
        &mut self,
        mode: OpusMode,
        samples_per_channel: usize,
        out: &mut [i16],
    ) -> Result<(), Error> {
        if mode != OpusMode::CeltOnly {
            self.silk.decode_lost(samples_per_channel, out)?;
        }
        if mode == OpusMode::SilkOnly {
            return Ok(());
        }
        let (start_band, output) = if mode == OpusMode::Hybrid {
            (HYBRID_CELT_START_BAND, CeltOutput::AddToSilk)
        } else {
            (0, CeltOutput::Replace)
        };
        self.celt.set_start_band(start_band);
        self.celt
            .decode_lost(samples_per_channel, out, output)
            .map(|_| ())
    }

    /// Vuelve al estado recién creado: reinicia CELT y SILK y olvida el paquete anterior.
    pub(crate) fn reset(&mut self) {
        self.celt.reset();
        self.silk.reset();
        self.last_packet_split_count = 0;
        self.last_final_range = 0;
        self.last_had_redundancy = false;
        self.last_celt_to_silk = false;
        self.prev_mode = None;
        self.prev_redundancy = false;
        self.loss_count = 0;
        self.last_packet_duration = 0;
        self.frame_size = self.samples_2_5ms();
        self.last_output.clear();
    }

    /// Estado final del decodificador de rango del último paquete, para comprobar la
    /// conformidad.
    pub(crate) fn final_range(&self) -> u32 {
        self.last_final_range
    }

    /// Particiones recursivas de la última trama CELT (para depurar).
    pub(crate) fn last_split_count(&self) -> usize {
        self.last_packet_split_count
    }

    /// Memoria del filtro de deénfasis del canal 0 (para depurar la síntesis CELT).
    pub(crate) fn deemph_mem(&self) -> f32 {
        self.celt.deemph_mem(0)
    }

    /// Si la última trama CELT era transitoria (para depurar).
    pub(crate) fn last_is_transient(&self) -> bool {
        false
    }

    /// Si el último paquete llevaba redundancia CELT en una trama SILK (para depurar).
    pub(crate) fn last_had_redundancy(&self) -> bool {
        self.last_had_redundancy
    }

    /// Si esa redundancia era de transición de CELT a SILK (para depurar).
    pub(crate) fn last_celt_to_silk(&self) -> bool {
        self.last_celt_to_silk
    }

    /// Decodifica un paquete Opus en PCM `i16` entrelazado y devuelve las muestras por canal;
    /// con `packet` a `None`, oculta una pérdida.
    ///
    /// # Errores
    ///
    /// `BadPacket` si el paquete no es válido, `OutputTooSmall` si no cabe en `out`, y los de
    /// cada capa.
    pub(crate) fn decode(
        &mut self,
        packet: Option<&[u8]>,
        out: &mut [i16],
    ) -> Result<usize, Error> {
        let parsed = packet.map(packet::parse_packet).transpose()?;
        let samples_per_channel_needed = parsed.as_ref().map_or(self.last_packet_duration, |pp| {
            pp.samples_per_channel(self.fs_hz)
        });

        let needed = samples_per_channel_needed * self.channels as usize;
        if out.len() < needed {
            return Err(Error::OutputTooSmall {
                needed,
                got: out.len(),
            });
        }

        let Some(pp) = parsed else {
            return self.decode_lost_packet(out);
        };

        let config = packet::toc_config(pp.toc);
        let mode = mode_for_config(config);
        self.last_packet_split_count = 0;
        self.last_final_range = 0;
        self.frame_size = pp.samples_per_frame_48k * self.fs_hz as usize / OPUS_RATE_HZ;

        let mut transition = self.packet_transition(mode);
        let ctx = PacketContext {
            config,
            packet_channels: pp.packet_channels,
            frame_samples_48k: pp.samples_per_frame_48k,
        };
        let channels = self.channels as usize;
        let mut last_frame = FrameOutcome::default();
        let mut written_per_channel = 0usize;
        let frame_samples = pp.samples_per_frame_48k * self.fs_hz as usize / OPUS_RATE_HZ;
        for &frame in pp.frames().iter() {
            let out_frame = &mut out[written_per_channel * channels..];
            if frame.len() <= MAX_DTX_FRAME_BYTES {
                // Como `opus_decode_frame` de libopus, una trama de un byte o ninguno (la DTX
                // manda así sus paquetes) se oculta como una pérdida, con el modo anterior y la
                // duración de la trama; el modo anterior no cambia.
                let samples_per_channel = self.conceal_samples(frame_samples, out_frame)?;
                written_per_channel += samples_per_channel;
                last_frame = FrameOutcome {
                    samples_per_channel,
                    ..FrameOutcome::default()
                };
                continue;
            }
            last_frame = match mode {
                OpusMode::CeltOnly => {
                    self.decode_celt_only_frame(frame, &ctx, &mut transition, out_frame)?
                }
                OpusMode::SilkOnly => self.decode_silk_only_frame(
                    frame,
                    &ctx,
                    &mut transition,
                    SilkPayload::Main,
                    out_frame,
                )?,
                OpusMode::Hybrid => {
                    self.decode_hybrid_frame(frame, &ctx, &mut transition, out_frame)?
                }
            };
            written_per_channel += last_frame.samples_per_channel;
            // Como `opus_decode_frame` de libopus, el modo y la redundancia anteriores son los de
            // la trama anterior, no los del paquete anterior: la segunda trama de un paquete que
            // entra en híbrido desde CELT ya no debe reiniciar CELT.
            self.prev_mode = Some(mode);
            self.prev_redundancy = last_frame.had_redundancy;
        }

        self.last_had_redundancy = last_frame.had_redundancy;
        self.last_celt_to_silk = last_frame.celt_to_silk;
        self.loss_count = 0;
        self.last_packet_duration = written_per_channel;
        self.store_last_output(out, written_per_channel);
        Ok(written_per_channel)
    }

    /// Recupera con FEC en banda el paquete perdido anterior a `packet`, como
    /// `opus_decode_native` de libopus con `decode_fec`: la pérdida dura lo que el último paquete
    /// decodificado. Si `packet` no puede traer FEC para ella (es solo CELT, lo era el modo
    /// anterior o la pérdida dura menos que una trama de `packet`), la oculta. Si no, oculta lo que
    /// sobre de la pérdida y decodifica las tramas LBRR de la primera trama de `packet` con su
    /// modo, su ancho de banda y sus canales, sin leer redundancia. En las tramas híbridas, la capa
    /// CELT no se reconstruye (como en la ocultación híbrida actual).
    ///
    /// # Errores
    ///
    /// `BadPacket` si `packet` no es válido, `OutputTooSmall` si la pérdida no cabe en `out`, y
    /// los de SILK.
    pub(crate) fn decode_fec(&mut self, packet: &[u8], out: &mut [i16]) -> Result<usize, Error> {
        let pp = packet::parse_packet(packet)?;
        let config = packet::toc_config(pp.toc);
        let mode = mode_for_config(config);
        let frame_size = self.last_packet_duration;
        let packet_frame_size = pp.samples_per_frame_48k * self.fs_hz as usize / OPUS_RATE_HZ;
        if frame_size < packet_frame_size
            || mode == OpusMode::CeltOnly
            || self.prev_mode == Some(OpusMode::CeltOnly)
        {
            return self.decode_lost_packet(out);
        }
        let channels = self.channels as usize;
        let needed = frame_size * channels;
        if out.len() < needed {
            return Err(Error::OutputTooSmall {
                needed,
                got: out.len(),
            });
        }
        let concealed = frame_size - packet_frame_size;
        if concealed > 0 {
            self.conceal_samples(concealed, out)?;
        }
        self.frame_size = packet_frame_size;

        let mut transition = self.packet_transition(mode);
        let ctx = PacketContext {
            config,
            packet_channels: pp.packet_channels,
            frame_samples_48k: pp.samples_per_frame_48k,
        };
        let payload = SilkPayload::Lbrr;
        let frame = pp.frames()[0];
        let out_frame = &mut out[concealed * channels..];
        match mode {
            OpusMode::SilkOnly => {
                self.decode_silk_only_frame(frame, &ctx, &mut transition, payload, out_frame)?;
            }
            // Un paquete solo CELT ya se ha ocultado arriba; no llega aquí.
            OpusMode::Hybrid | OpusMode::CeltOnly => {
                self.decode_hybrid_fec_frame(frame, &ctx, &mut transition, payload, out_frame)?;
            }
        }

        self.prev_mode = Some(mode);
        self.prev_redundancy = false;
        self.last_had_redundancy = false;
        self.last_celt_to_silk = false;
        self.last_packet_split_count = 0;
        self.loss_count = 0;
        self.last_packet_duration = frame_size;
        self.store_last_output(out, frame_size);
        Ok(frame_size)
    }

    /// Recupera la capa SILK de una trama híbrida con sus tramas LBRR (sin redundancia). La capa
    /// CELT no se reconstruye: queda la salida SILK sola, como en la ocultación híbrida actual.
    ///
    /// # Errores
    ///
    /// Los de SILK.
    fn decode_hybrid_fec_frame(
        &mut self,
        frame: &[u8],
        ctx: &PacketContext,
        transition: &mut PacketTransition,
        payload: SilkPayload,
        out_frame: &mut [i16],
    ) -> Result<usize, Error> {
        self.reset_silk_after_celt(transition);
        let mut ec = EcDec::new(frame);
        let samples_per_channel =
            self.silk
                .decode_payload(&mut ec, ctx.silk_request(), payload, out_frame)?;
        self.silk.finish(&ec)?;
        self.last_final_range = self.silk.final_range();
        Ok(samples_per_channel)
    }

    /// Decide qué transiciones de modo aplica este paquete (`opus_decode_frame`): al entrar en
    /// CELT desde otro modo sin redundancia, o al salir de CELT, la primera trama se funde con la
    /// ocultación del modo anterior (en SILK o híbrido, solo si la trama no trae redundancia); al
    /// salir de CELT, además, se reinicia SILK.
    fn packet_transition(&self, mode: OpusMode) -> PacketTransition {
        let transition = self.prev_mode.is_some()
            && ((mode == OpusMode::CeltOnly
                && self.prev_mode != Some(OpusMode::CeltOnly)
                && !self.prev_redundancy)
                || (mode != OpusMode::CeltOnly && self.prev_mode == Some(OpusMode::CeltOnly)));
        PacketTransition {
            pending: transition,
            reset_silk: transition && self.prev_mode == Some(OpusMode::CeltOnly),
        }
    }

    /// Ocultación con el modo anterior con que libopus funde la primera trama tras un cambio de
    /// modo (`pcm_transition`): `min(F5, frame_samples)` muestras por canal, como su llamada a
    /// `opus_decode_frame` con `data == NULL`, que oculta con el modo anterior (o CELT si traía
    /// redundancia) y no cambia ni ese modo ni la cuenta de pérdidas.
    ///
    /// # Errores
    ///
    /// Los de la ocultación de SILK y de CELT.
    fn conceal_transition(&mut self, frame_samples: usize) -> Result<Vec<i16>, Error> {
        let samples = self.samples_5ms().min(frame_samples);
        let mut audio = vec![0i16; samples * self.channels as usize];
        let mode = if self.prev_redundancy {
            Some(OpusMode::CeltOnly)
        } else {
            self.prev_mode
        };
        if let Some(mode) = mode {
            self.conceal_chunk(mode, samples, &mut audio)?;
        }
        Ok(audio)
    }

    /// Funde el principio de `out_frame`, de `frame_samples` muestras por canal, con la
    /// ocultación `audio` de [`Self::conceal_transition`] ([`fade_mode_transition_i16`]).
    fn fade_mode_transition(&self, audio: &[i16], frame_samples: usize, out_frame: &mut [i16]) {
        fade_mode_transition_i16(
            audio,
            out_frame,
            frame_samples,
            self.samples_2_5ms(),
            self.channels as usize,
            self.celt.window(),
            self.fs_hz,
        );
    }

    /// Decodifica una trama solo CELT y, si es la primera tras un cambio de modo, la funde con la
    /// ocultación del modo anterior, generada antes de reiniciar CELT.
    fn decode_celt_only_frame(
        &mut self,
        frame: &[u8],
        ctx: &PacketContext,
        transition: &mut PacketTransition,
        out_frame: &mut [i16],
    ) -> Result<FrameOutcome, Error> {
        let transition_audio = if std::mem::take(&mut transition.pending) {
            let audio = self.conceal_transition(self.frame_size)?;
            self.celt.reset();
            Some(audio)
        } else {
            None
        };
        // La duración de la trama CELT va en muestras a 48 kHz, que es lo que necesita CELT para
        // elegir `lm`.
        let mut ec = EcDec::new(frame);
        let celt_frame = self.celt.decode_frame_with_ec(
            &mut ec,
            ctx.frame_samples_48k,
            ctx.config,
            ctx.packet_channels,
            out_frame,
            CeltOutput::Replace,
        )?;
        if let Some(audio) = &transition_audio {
            self.fade_mode_transition(audio, celt_frame.samples_per_channel, out_frame);
        }
        self.last_packet_split_count += self.celt.last_split_count();
        self.last_final_range = self.celt.final_range();
        Ok(FrameOutcome {
            samples_per_channel: celt_frame.samples_per_channel,
            had_redundancy: false,
            celt_to_silk: false,
        })
    }

    /// Decodifica una trama solo SILK, la carga principal o sus tramas LBRR según `payload`, y,
    /// con la principal, la redundancia CELT que pueda llevar al final.
    fn decode_silk_only_frame(
        &mut self,
        frame: &[u8],
        ctx: &PacketContext,
        transition: &mut PacketTransition,
        payload: SilkPayload,
        out_frame: &mut [i16],
    ) -> Result<FrameOutcome, Error> {
        self.reset_silk_after_celt(transition);
        let mut ec = EcDec::new(frame);
        let samples_per_channel =
            self.silk
                .decode_payload(&mut ec, ctx.silk_request(), payload, out_frame)?;
        // La cola se lee antes de cerrar SILK: su estado final de rango incluye esa lectura. Con
        // FEC no se lee (`opus_decode_frame` solo la busca si no es `decode_fec`).
        let redundancy = match payload {
            SilkPayload::Main => read_silk_only_redundancy(&mut ec, frame),
            SilkPayload::Lbrr => FrameRedundancy::NONE,
        };
        self.silk.finish(&ec)?;
        // La ocultación de la transición desde CELT, solo sin redundancia, va antes que la
        // redundancia y que la extinción de CELT (`opus_decode_frame`).
        let transition_audio = self.take_transition_audio(transition, redundancy.present)?;
        // Mismo orden que `opus_decode_frame`: la redundancia CELT→SILK se decodifica antes de
        // extinguir CELT, y la SILK→CELT, que reinicia CELT, después.
        let redundancy_data = &frame[frame.len() - redundancy.bytes..];
        let celt_to_silk = redundancy.present && redundancy.celt_to_silk;
        let mut decoded = if celt_to_silk {
            Some(self.decode_silk_only_redundancy(redundancy_data, ctx, &redundancy)?)
        } else {
            None
        };
        if self.prev_mode == Some(OpusMode::Hybrid) && !(celt_to_silk && self.prev_redundancy) {
            self.fade_out_celt_after_hybrid(ctx, out_frame)?;
        }
        if redundancy.present && !redundancy.celt_to_silk {
            decoded = Some(self.decode_silk_only_redundancy(redundancy_data, ctx, &redundancy)?);
        }
        // La CELT→SILK se decodifica siempre (estado de CELT y de rango), pero solo se mezcla en
        // la salida si la trama anterior no era solo SILK, como en el camino híbrido.
        let mixed =
            !celt_to_silk || celt_to_silk_redundancy_is_mixed(self.prev_mode, self.prev_redundancy);
        let mut redundancy_rng = 0u32;
        if let Some(decoded) = &decoded {
            if mixed {
                self.apply_silk_only_redundancy(
                    decoded,
                    &redundancy,
                    samples_per_channel,
                    out_frame,
                );
            }
            redundancy_rng = decoded.final_range;
        }
        if let Some(audio) = &transition_audio {
            self.fade_mode_transition(audio, samples_per_channel, out_frame);
        }
        self.last_final_range = self.silk.final_range() ^ redundancy_rng;
        Ok(FrameOutcome {
            samples_per_channel,
            had_redundancy: redundancy.present && !redundancy.celt_to_silk,
            celt_to_silk: redundancy.celt_to_silk,
        })
    }

    /// Al pasar de híbrido a solo SILK, deja que la MDCT de CELT se extinga: decodifica una trama
    /// CELT de silencio de 2,5 ms desde la banda 0 y la suma al principio de `out_frame`, como
    /// `opus_decode_frame` de libopus. No cambia el `final_range` de la trama, que es el de SILK.
    ///
    /// # Errores
    ///
    /// Los de [`celt::CeltDecoder::decode_frame_with_ec`].
    fn fade_out_celt_after_hybrid(
        &mut self,
        ctx: &PacketContext,
        out_frame: &mut [i16],
    ) -> Result<(), Error> {
        self.celt.set_start_band(0);
        self.celt.set_end_band(packet::celt_end_band(ctx.config));
        let mut ec = EcDec::new(&CELT_SILENCE_FRAME);
        let result = self.celt.decode_frame_with_ec(
            &mut ec,
            CELT_FADE_OUT_SAMPLES_48K,
            ctx.config,
            ctx.packet_channels,
            out_frame,
            CeltOutput::AddToSilk,
        );
        self.celt.clear_end_band();
        result.map(|_| ())
    }

    /// Decodifica la trama CELT redundante de 5 ms de una trama solo SILK. Si la transición es de
    /// SILK a CELT, reinicia antes CELT, como libopus.
    ///
    /// # Errores
    ///
    /// Los de [`celt::CeltDecoder::decode_frame`].
    fn decode_silk_only_redundancy(
        &mut self,
        redundancy_data: &[u8],
        ctx: &PacketContext,
        redundancy: &FrameRedundancy,
    ) -> Result<DecodedRedundancy, Error> {
        let mut audio = vec![0i16; self.samples_5ms() * self.channels as usize];
        if !redundancy.celt_to_silk {
            self.celt.reset();
        }
        self.celt.set_start_band(0);
        self.celt.set_end_band(packet::celt_end_band(ctx.config));
        let redundancy_frame = self.celt.decode_frame(
            redundancy_data,
            REDUNDANT_FRAME_SAMPLES_48K,
            ctx.config,
            ctx.packet_channels,
            &mut audio,
        )?;
        self.celt.clear_end_band();
        Ok(DecodedRedundancy {
            audio,
            samples_per_channel: redundancy_frame.samples_per_channel,
            final_range: self.celt.final_range(),
        })
    }

    /// Funde la redundancia ya decodificada con la trama solo SILK de `silk_samples` muestras por
    /// canal: al final si la transición es de SILK a CELT, al principio si es de CELT a SILK.
    fn apply_silk_only_redundancy(
        &self,
        decoded: &DecodedRedundancy,
        redundancy: &FrameRedundancy,
        silk_samples: usize,
        out_frame: &mut [i16],
    ) {
        let channels = self.channels as usize;
        let overlap = self.samples_2_5ms();
        if !redundancy.celt_to_silk {
            let silk_tail_start = (silk_samples - overlap) * channels;
            let silk_tail_end = silk_samples * channels;
            let redundancy_start = overlap * channels;
            let redundancy_end = redundancy_start + overlap * channels;
            let silk_tail = out_frame[silk_tail_start..silk_tail_end].to_vec();
            smooth_fade_i16(
                &silk_tail,
                &decoded.audio[redundancy_start..redundancy_end],
                &mut out_frame[silk_tail_start..silk_tail_end],
                overlap,
                channels,
                self.celt.window(),
                self.fs_hz,
            );
        } else {
            let frame_len = silk_samples * channels;
            apply_transition_fade_i16(
                &decoded.audio,
                &mut out_frame[..frame_len],
                overlap.min(decoded.samples_per_channel / 2),
                channels,
                self.celt.window(),
                self.fs_hz,
            );
        }
    }

    /// Reinicia SILK en la primera trama de un paquete solo SILK o híbrido que sigue a uno solo
    /// CELT, como `opus_decode_frame` de libopus (`silk_ResetDecoder` si `prev_mode` era
    /// `MODE_CELT_ONLY`).
    fn reset_silk_after_celt(&mut self, transition: &mut PacketTransition) {
        if transition.reset_silk {
            self.silk.reset();
            transition.reset_silk = false;
        }
    }

    /// En la primera trama solo SILK o híbrida de un paquete que sale de CELT, la ocultación de
    /// CELT con que se funde, salvo si la trama trae redundancia (`opus_decode_frame` anula
    /// entonces la transición).
    ///
    /// # Errores
    ///
    /// Los de la ocultación de CELT.
    fn take_transition_audio(
        &mut self,
        transition: &mut PacketTransition,
        has_redundancy: bool,
    ) -> Result<Option<Vec<i16>>, Error> {
        if !std::mem::take(&mut transition.pending) || has_redundancy {
            return Ok(None);
        }
        self.conceal_transition(self.frame_size).map(Some)
    }

    /// Decodifica una trama híbrida: SILK (reiniciado si el paquete anterior era solo CELT), la
    /// cabecera de redundancia, la redundancia CELT→SILK si la hay, la capa CELT desde la banda 17
    /// y, por último, la redundancia SILK→CELT.
    fn decode_hybrid_frame(
        &mut self,
        frame: &[u8],
        ctx: &PacketContext,
        transition: &mut PacketTransition,
        out_frame: &mut [i16],
    ) -> Result<FrameOutcome, Error> {
        self.reset_silk_after_celt(transition);
        let channels = self.channels as usize;
        let mut ec = EcDec::new(frame);
        let silk_samples =
            self.silk
                .decode_frame_with_ec(&mut ec, ctx.silk_request(), out_frame)?;
        let header = read_hybrid_redundancy(&mut ec, frame);
        let redundancy_data = &frame[frame.len() - header.bytes..];
        let transition_audio = self.take_transition_audio(transition, header.present)?;

        let mut celt_to_silk_audio = Vec::new();
        let mut celt_to_silk_samples = 0usize;
        let mut redundant_rng = 0u32;
        let apply_celt_to_silk_audio = header.present
            && header.celt_to_silk
            && celt_to_silk_redundancy_is_mixed(self.prev_mode, self.prev_redundancy);
        let reset_main_celt = self.prev_mode.is_some()
            && self.prev_mode != Some(OpusMode::Hybrid)
            && !self.prev_redundancy;
        if header.present && header.celt_to_silk {
            self.celt.set_start_band(0);
            celt_to_silk_audio = vec![0i16; self.samples_5ms() * channels];
            let redundancy_frame = self.celt.decode_frame(
                redundancy_data,
                REDUNDANT_FRAME_SAMPLES_48K,
                ctx.config,
                ctx.packet_channels,
                &mut celt_to_silk_audio,
            )?;
            self.last_packet_split_count += self.celt.last_split_count();
            celt_to_silk_samples = redundancy_frame.samples_per_channel;
            redundant_rng = self.celt.final_range();
        }
        if reset_main_celt {
            self.celt.reset();
        }
        self.celt.set_start_band(HYBRID_CELT_START_BAND);
        let celt_frame = match self.celt.decode_frame_with_ec(
            &mut ec,
            ctx.frame_samples_48k,
            ctx.config,
            ctx.packet_channels,
            out_frame,
            CeltOutput::AddToSilk,
        ) {
            Ok(frame) => frame,
            Err(err) => {
                self.celt.set_start_band(0);
                return Err(err);
            }
        };
        self.celt.set_start_band(0);
        debug_assert_eq!(silk_samples, celt_frame.samples_per_channel);
        self.last_packet_split_count += self.celt.last_split_count();
        self.last_final_range = self.celt.final_range();
        if header.present && !header.celt_to_silk {
            let frame_len = celt_frame.samples_per_channel * channels;
            self.last_final_range ^= self.apply_hybrid_silk_to_celt_redundancy(
                redundancy_data,
                ctx,
                frame_len,
                out_frame,
            )?;
        }
        if header.present && header.celt_to_silk {
            if apply_celt_to_silk_audio {
                let frame_len = celt_frame.samples_per_channel * channels;
                apply_transition_fade_i16(
                    &celt_to_silk_audio,
                    &mut out_frame[..frame_len],
                    self.samples_2_5ms().min(celt_to_silk_samples / 2),
                    channels,
                    self.celt.window(),
                    self.fs_hz,
                );
            }
            self.last_final_range ^= redundant_rng;
        }
        if let Some(audio) = &transition_audio {
            self.fade_mode_transition(audio, silk_samples, out_frame);
        }
        Ok(FrameOutcome {
            samples_per_channel: silk_samples,
            had_redundancy: header.present && !header.celt_to_silk,
            celt_to_silk: header.celt_to_silk,
        })
    }

    /// Decodifica la redundancia SILK→CELT de una trama híbrida y funde con ella el final de la
    /// trama. Devuelve su `final_range`.
    fn apply_hybrid_silk_to_celt_redundancy(
        &mut self,
        redundancy_data: &[u8],
        ctx: &PacketContext,
        frame_len: usize,
        out_frame: &mut [i16],
    ) -> Result<u32, Error> {
        let channels = self.channels as usize;
        let redundancy_samples = self.samples_5ms();
        let overlap = self.samples_2_5ms();
        let mut redundancy_out = vec![0i16; redundancy_samples * channels];
        self.celt.reset();
        self.celt.set_start_band(0);
        let redundancy_frame = self.celt.decode_frame(
            redundancy_data,
            REDUNDANT_FRAME_SAMPLES_48K,
            ctx.config,
            ctx.packet_channels,
            &mut redundancy_out,
        )?;
        self.last_packet_split_count += self.celt.last_split_count();
        let redundant_rng = self.celt.final_range();
        let fade_len = overlap * channels;
        let tail_start = frame_len.saturating_sub(fade_len);
        let tail_end = tail_start + fade_len;
        let redundancy_start = fade_len;
        let redundancy_end = redundancy_start + fade_len;
        if tail_end <= out_frame.len()
            && redundancy_end <= redundancy_out.len()
            && redundancy_frame.samples_per_channel == redundancy_samples
        {
            let celt_tail = out_frame[tail_start..tail_end].to_vec();
            smooth_fade_i16(
                &celt_tail,
                &redundancy_out[redundancy_start..redundancy_end],
                &mut out_frame[tail_start..tail_end],
                overlap,
                channels,
                self.celt.window(),
                self.fs_hz,
            );
        }
        Ok(redundant_rng)
    }
}

/// Lo que comparten todas las tramas de un paquete.
struct PacketContext {
    /// Configuración del TOC (RFC 6716 §3.1).
    config: u8,
    /// Canales codificados en el paquete (bit `s` del TOC).
    packet_channels: u8,
    /// Duración de cada trama, en muestras a 48 kHz.
    frame_samples_48k: usize,
}

impl PacketContext {
    /// Lo que la capa SILK necesita del TOC para decodificar una trama.
    fn silk_request(&self) -> silk::SilkFrameRequest {
        silk::SilkFrameRequest {
            frame_samples_48k: self.frame_samples_48k,
            config: self.config,
            packet_channels: self.packet_channels,
        }
    }
}

/// Trama CELT redundante de 5 ms ya decodificada, pendiente de fundirse con la trama.
struct DecodedRedundancy {
    /// PCM entrelazado de la redundancia.
    audio: Vec<i16>,
    /// Muestras por canal que dio la redundancia.
    samples_per_channel: usize,
    /// Estado final del decodificador de rango de la redundancia.
    final_range: u32,
}

/// Transiciones de modo pendientes dentro de un paquete; solo afectan a su primera trama.
struct PacketTransition {
    /// Cambio de modo sin redundancia previa (`transition` de `opus_decode_frame`): la primera
    /// trama aún debe fundirse con la ocultación del modo anterior.
    pending: bool,
    /// La primera trama SILK aún debe reiniciar SILK.
    reset_silk: bool,
}

/// Resultado de una trama, del que el paquete conserva el de la última.
#[derive(Default)]
struct FrameOutcome {
    samples_per_channel: usize,
    /// La trama llevaba redundancia SILK→CELT.
    had_redundancy: bool,
    /// La redundancia, si la había, era de CELT a SILK.
    celt_to_silk: bool,
}

/// Cola o cabecera de redundancia de una trama solo SILK o híbrida (RFC 6716 §4.5.1).
struct FrameRedundancy {
    /// La trama lleva una trama CELT redundante.
    present: bool,
    /// Sentido de la transición que anuncia la redundancia (de CELT a SILK).
    celt_to_silk: bool,
    /// Bytes de redundancia al final de la trama; 0 si no hay.
    bytes: usize,
}

impl FrameRedundancy {
    /// Sin redundancia.
    const NONE: Self = Self {
        present: false,
        celt_to_silk: false,
        bytes: 0,
    };
}

/// Muestras por canal que oculta una llamada a `opus_decode_frame` de libopus con `data == NULL`
/// cuando faltan `remaining` (ya limitadas a una trama del último paquete) a `fs_hz` en el modo
/// `mode`: como mucho 20 ms; por debajo, 10 ms si pasa de 10, y fuera de SILK, 5 ms si está entre
/// 5 y 10; si no, todo. Así 12,5 ms se ocultan como 10 + 2,5, que son duraciones de CELT.
fn concealment_chunk(remaining: usize, mode: OpusMode, fs_hz: u32) -> usize {
    let fs = fs_hz as usize;
    let f20 = fs / TWENTY_MS_PER_SECOND;
    let f10 = fs / TEN_MS_PER_SECOND;
    let f5 = fs / FIVE_MS_PER_SECOND;
    if remaining >= f20 {
        f20
    } else if remaining > f10 {
        f10
    } else if mode != OpusMode::SilkOnly && remaining > f5 && remaining < f10 {
        f5
    } else {
        remaining
    }
}

/// Si la trama redundante CELT→SILK de un paquete se mezcla en la salida, como
/// `opus_decode_frame` de libopus (`src/opus_decoder.c`): solo si la trama anterior no era solo
/// SILK o traía redundancia SILK→CELT. Tras ocultar una pérdida en SILK, el modo anterior es SILK
/// y no se mezcla, aunque la trama redundante se decodifique igual. `prev_mode` y
/// `prev_redundancy` son los de la trama anterior.
fn celt_to_silk_redundancy_is_mixed(prev_mode: Option<OpusMode>, prev_redundancy: bool) -> bool {
    prev_mode != Some(OpusMode::SilkOnly) || prev_redundancy
}

/// Modo del paquete según su configuración (RFC 6716 §3.1, Tabla 2).
fn mode_for_config(config: u8) -> OpusMode {
    if config <= LAST_SILK_ONLY_CONFIG {
        OpusMode::SilkOnly
    } else if config <= LAST_HYBRID_CONFIG {
        OpusMode::Hybrid
    } else {
        OpusMode::CeltOnly
    }
}

/// Lee la cola de redundancia de una trama solo SILK (`opus_decode_frame`): si quedan al menos
/// `REDUNDANCY_MIN_BITS`, un bit de sentido y, en los bytes que queden, la trama CELT redundante.
/// El sentido se lee aunque no quede ningún byte para la redundancia.
fn read_silk_only_redundancy(ec: &mut EcDec<'_>, frame: &[u8]) -> FrameRedundancy {
    if ec.tell() + REDUNDANCY_MIN_BITS > (frame.len() * BITS_PER_BYTE) as i32 {
        return FrameRedundancy {
            present: false,
            celt_to_silk: false,
            bytes: 0,
        };
    }
    let celt_to_silk = ec.dec_bit_logp(1);
    let bytes = frame
        .len()
        .saturating_sub((ec.tell().max(0) as usize).div_ceil(BITS_PER_BYTE));
    FrameRedundancy {
        present: bytes > 0,
        celt_to_silk,
        bytes,
    }
}

/// Lee la cabecera de redundancia de una trama híbrida (`opus_decode_frame`) y recorta el
/// decodificador de rango para que la capa CELT no lea los bytes de la redundancia.
fn read_hybrid_redundancy(ec: &mut EcDec<'_>, frame: &[u8]) -> FrameRedundancy {
    let mut redundancy = if ec.tell() + REDUNDANCY_MIN_BITS + REDUNDANCY_HYBRID_EXTRA_BITS
        <= (frame.len() * BITS_PER_BYTE) as i32
    {
        ec.dec_bit_logp(REDUNDANCY_FLAG_LOGP)
    } else {
        false
    };
    let mut celt_to_silk = false;
    let mut redundancy_bytes = 0usize;
    if redundancy {
        celt_to_silk = ec.dec_bit_logp(1);
        redundancy_bytes = ec.dec_uint(REDUNDANCY_SIZE_RANGE) as usize + REDUNDANCY_MIN_BYTES;
        // Comprobación de cordura de libopus (`opus_decode_frame`): si la
        // redundancia anunciada no cabe tras lo ya leído, se descarta. Un paquete
        // válido nunca llega aquí y libopus declara no normativo lo que sigue;
        // libopus además oculta la capa CELT de esta trama, y aquí se decodifica
        // con lo que quede. Sin esta comprobación, `frame.len() -
        // redundancy_bytes` desbordaba.
        let cabe = frame
            .len()
            .checked_sub(redundancy_bytes)
            .is_some_and(|resto| {
                i64::try_from(resto * BITS_PER_BYTE).unwrap_or(i64::MAX) >= i64::from(ec.tell())
            });
        if !cabe {
            redundancy = false;
            redundancy_bytes = 0;
        }
        ec.shrink_storage(redundancy_bytes);
    }
    FrameRedundancy {
        present: redundancy,
        celt_to_silk,
        bytes: redundancy_bytes,
    }
}

/// Funde `in1` con `in2` en `out` a lo largo de `overlap` muestras por canal, con el cuadrado de
/// la ventana de solapamiento de CELT (`smooth_fade`), muestreada a la frecuencia `fs_hz`.
fn smooth_fade_i16(
    in1: &[i16],
    in2: &[i16],
    out: &mut [i16],
    overlap: usize,
    channels: usize,
    window: &[f32],
    fs_hz: u32,
) {
    let inc = OPUS_RATE_HZ / fs_hz as usize;
    for c in 0..channels {
        for i in 0..overlap {
            let w = window[i * inc] * window[i * inc];
            let idx = i * channels + c;
            out[idx] = saturate_to_i16(w * in2[idx] as f32 + (1.0 - w) * in1[idx] as f32);
        }
    }
}

/// Transición de un cambio de modo: las `overlap` primeras muestras por canal de `pcm` pasan a
/// ser las de `transition`, y las siguientes, hasta `overlap`, se funden de `transition` a `pcm`.
fn apply_transition_fade_i16(
    transition: &[i16],
    pcm: &mut [i16],
    overlap: usize,
    channels: usize,
    window: &[f32],
    fs_hz: u32,
) {
    if overlap == 0 || channels == 0 {
        return;
    }

    let prefix_len = overlap * channels;
    let copy_len = prefix_len.min(transition.len()).min(pcm.len());
    pcm[..copy_len].copy_from_slice(&transition[..copy_len]);

    let fade_available = (transition.len().saturating_sub(prefix_len))
        .min(pcm.len().saturating_sub(prefix_len))
        / channels;
    if fade_available == 0 {
        return;
    }

    let fade_samples = fade_available.min(overlap);
    let fade_len = fade_samples * channels;
    let fade_start = prefix_len;
    let fade_end = fade_start + fade_len;
    let incoming = pcm[fade_start..fade_end].to_vec();
    smooth_fade_i16(
        &transition[fade_start..fade_end],
        &incoming,
        &mut pcm[fade_start..fade_end],
        fade_samples,
        channels,
        window,
        fs_hz,
    );
}

/// Fundido de la primera trama tras un cambio de modo sin redundancia con la ocultación del modo
/// anterior `transition`, como `opus_decode_frame`: si la trama, de `frame_samples` muestras por
/// canal, dura 5 ms (dos veces `overlap`, que son 2,5 ms) o más, sus `overlap` primeras muestras
/// por canal pasan a ser las de `transition` y las `overlap` siguientes se funden; si dura menos
/// (tramas CELT de 2,5 ms), se funden sus `overlap` primeras sin copiar nada.
fn fade_mode_transition_i16(
    transition: &[i16],
    pcm: &mut [i16],
    frame_samples: usize,
    overlap: usize,
    channels: usize,
    window: &[f32],
    fs_hz: u32,
) {
    if frame_samples >= 2 * overlap {
        apply_transition_fade_i16(
            transition,
            &mut pcm[..frame_samples * channels],
            overlap,
            channels,
            window,
            fs_hz,
        );
        return;
    }
    let fade_len = overlap * channels;
    let incoming = pcm[..fade_len].to_vec();
    smooth_fade_i16(
        &transition[..fade_len],
        &incoming,
        &mut pcm[..fade_len],
        overlap,
        channels,
        window,
        fs_hz,
    );
}

#[cfg(test)]
mod tests {
    use super::{
        OpusMode, celt_to_silk_redundancy_is_mixed, concealment_chunk, fade_mode_transition_i16,
    };

    const FS_48K_HZ: u32 = 48_000;
    /// Duraciones a 48 kHz, en muestras por canal.
    const MS_2_5: usize = 120;
    const MS_5: usize = 240;
    const MS_7_5: usize = 360;
    const MS_10: usize = 480;
    const MS_12_5: usize = 600;
    const MS_20: usize = 960;
    const MS_25: usize = 1200;

    /// Ventana constante para las pruebas del fundido: su cuadrado, 0,25, es el peso de la trama
    /// nueva en cada muestra fundida.
    const FADE_TEST_WINDOW: [f32; MS_2_5] = [0.5; MS_2_5];
    const FADE_TEST_TRANSITION: i16 = 1000;
    const FADE_TEST_FRAME: i16 = 2000;
    /// 0,25 · 2000 + 0,75 · 1000.
    const FADE_TEST_MIXED: i16 = 1250;

    #[test]
    fn transition_into_frames_of_5_ms_or_more_copies_then_fades() {
        for frame in [MS_5, MS_10] {
            let transition = vec![FADE_TEST_TRANSITION; MS_5 * 2];
            let mut pcm = vec![FADE_TEST_FRAME; frame * 2];
            fade_mode_transition_i16(
                &transition,
                &mut pcm,
                frame,
                MS_2_5,
                2,
                &FADE_TEST_WINDOW,
                FS_48K_HZ,
            );
            assert!(pcm[..MS_2_5 * 2].iter().all(|&s| s == FADE_TEST_TRANSITION));
            assert!(
                pcm[MS_2_5 * 2..MS_5 * 2]
                    .iter()
                    .all(|&s| s == FADE_TEST_MIXED)
            );
            assert!(pcm[MS_5 * 2..].iter().all(|&s| s == FADE_TEST_FRAME));
        }
    }

    #[test]
    fn transition_into_frames_of_2_5_ms_fades_without_copying() {
        let transition = vec![FADE_TEST_TRANSITION; MS_2_5 * 2];
        let mut pcm = vec![FADE_TEST_FRAME; MS_2_5 * 2];
        fade_mode_transition_i16(
            &transition,
            &mut pcm,
            MS_2_5,
            MS_2_5,
            2,
            &FADE_TEST_WINDOW,
            FS_48K_HZ,
        );
        assert!(pcm.iter().all(|&s| s == FADE_TEST_MIXED));
    }

    /// Trozos en que libopus oculta una pérdida de `total` muestras por canal.
    fn chunks(total: usize, mode: OpusMode) -> Vec<usize> {
        let mut out = Vec::new();
        let mut written = 0;
        while written < total {
            let chunk = concealment_chunk(total - written, mode, FS_48K_HZ);
            out.push(chunk);
            written += chunk;
        }
        out
    }

    /// Como `opus_decode_native` y `opus_decode_frame` de libopus: cada trozo es una duración que
    /// CELT puede ocultar.
    #[test]
    fn celt_losses_are_concealed_in_celt_frame_sizes() {
        assert_eq!(chunks(MS_12_5, OpusMode::CeltOnly), [MS_10, MS_2_5]);
        assert_eq!(chunks(MS_7_5, OpusMode::CeltOnly), [MS_5, MS_2_5]);
        assert_eq!(chunks(MS_25, OpusMode::CeltOnly), [MS_20, MS_5]);
        assert_eq!(chunks(3 * MS_20, OpusMode::Hybrid), [MS_20, MS_20, MS_20]);
    }

    /// En SILK no se baja a 5 ms.
    #[test]
    fn silk_losses_are_not_split_below_ten_ms() {
        assert_eq!(chunks(MS_7_5, OpusMode::SilkOnly), [MS_7_5]);
        assert_eq!(chunks(2 * MS_20, OpusMode::SilkOnly), [MS_20, MS_20]);
    }

    /// `redundancy && celt_to_silk && (st->prev_mode != MODE_SILK_ONLY || st->prev_redundancy)`
    /// de `opus_decode_frame`, con todos los modos anteriores (también ninguno) y redundancias.
    #[test]
    fn celt_to_silk_redundancy_is_mixed_like_libopus() {
        let modos = [
            None,
            Some(OpusMode::SilkOnly),
            Some(OpusMode::Hybrid),
            Some(OpusMode::CeltOnly),
        ];
        for prev_mode in modos {
            for prev_redundancy in [false, true] {
                let libopus = prev_mode != Some(OpusMode::SilkOnly) || prev_redundancy;
                assert_eq!(
                    celt_to_silk_redundancy_is_mixed(prev_mode, prev_redundancy),
                    libopus,
                    "{prev_mode:?}, {prev_redundancy}"
                );
            }
        }
        assert!(!celt_to_silk_redundancy_is_mixed(
            Some(OpusMode::SilkOnly),
            false
        ));
        assert!(celt_to_silk_redundancy_is_mixed(
            Some(OpusMode::CeltOnly),
            false
        ));
    }
}

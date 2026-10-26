//! Decodificador de un flujo Opus: elige el modo de cada paquete (SILK, híbrido o CELT), encadena
//! las capas, decodifica la redundancia de las transiciones y oculta los paquetes perdidos. La API
//! pública (`OpusDecoder`) vive en `lib.rs` y delega aquí.

use crate::celt::CeltOutput;
use crate::entropy::EcDec;
use crate::packet::{self, BITS_PER_BYTE, LAST_HYBRID_CONFIG, LAST_SILK_ONLY_CONFIG, OPUS_RATE_HZ};
use crate::pcm::{SUPPORTED_RATES_HZ, saturate_to_i16};
use crate::{Error, celt, silk};

/// Atenuación por paquete perdido de la ocultación sin CELT, y número máximo de pérdidas que
/// acumula. Son valores de la implementación original; no se han cotejado con libopus.
const CONCEAL_FADE_PER_LOSS: f32 = 0.9;
const CONCEAL_MAX_FADE_STEPS: u32 = 10;
/// Tramas de 5 ms y de 2,5 ms por segundo. En libopus, `F5` es la duración de la trama CELT
/// redundante de una transición y `F2_5`, la del fundido con ella (`opus_decode_frame`).
const FIVE_MS_PER_SECOND: usize = 200;
const TWO_AND_HALF_MS_PER_SECOND: usize = 400;
/// La trama redundante de 5 ms, a 48 kHz.
const REDUNDANT_FRAME_SAMPLES_48K: usize = OPUS_RATE_HZ / FIVE_MS_PER_SECOND;
/// Bits que deben quedar en la trama para leer la señal de redundancia: 17, y 20 más en híbrido
/// (`ec_tell(&dec)+17+20*(mode == MODE_HYBRID) <= 8*len` en `opus_decode_frame`).
pub(crate) const REDUNDANCY_MIN_BITS: i32 = 17;
const REDUNDANCY_HYBRID_EXTRA_BITS: i32 = 20;
/// La señal de redundancia híbrida tiene probabilidad 1/2^12 (`ec_dec_bit_logp(&dec, 12)`).
const REDUNDANCY_FLAG_LOGP: u32 = 12;
/// La longitud de la redundancia híbrida es uniforme en `0..256`, más 2 bytes
/// (`ec_dec_uint(&dec, 256)+2`).
const REDUNDANCY_SIZE_RANGE: u32 = 256;
const REDUNDANCY_MIN_BYTES: usize = 2;
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
        let samples_per_channel = self.last_packet_duration;
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
        match self.prev_mode {
            Some(OpusMode::CeltOnly) => {
                let concealed = self
                    .celt
                    .decode_lost(samples_per_channel, self.channels as usize);
                for (dst, &src) in out[..needed].iter_mut().zip(concealed.iter()) {
                    *dst = saturate_to_i16(src);
                }
            }
            Some(OpusMode::SilkOnly | OpusMode::Hybrid) => {
                self.silk
                    .decode_lost(samples_per_channel, out, self.loss_count)?;
            }
            _ => {
                self.conceal_with_fade(samples_per_channel, self.loss_count, out);
            }
        }

        self.loss_count = self.loss_count.saturating_add(1);
        self.last_packet_split_count = 0;
        self.last_final_range = 0;
        self.last_had_redundancy = false;
        self.last_celt_to_silk = false;
        self.prev_redundancy = false;
        self.store_last_output(out, samples_per_channel);
        Ok(samples_per_channel)
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
    /// `BadPacket` o `PacketTooLarge` si el paquete no es válido, `OutputTooSmall` si no cabe en
    /// `out`, y los de cada capa.
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

        self.celt.reset_loss_count();
        let config = packet::toc_config(pp.toc);
        let mode = mode_for_config(config);
        self.last_packet_split_count = 0;
        self.last_final_range = 0;

        let mut transition = self.packet_transition(mode);
        let ctx = PacketContext {
            config,
            packet_channels: pp.packet_channels,
            frame_samples_48k: pp.samples_per_frame_48k,
        };
        let channels = self.channels as usize;
        let mut last_frame = FrameOutcome::default();
        let mut written_per_channel = 0usize;
        for &frame in pp.frames().iter() {
            let out_frame = &mut out[written_per_channel * channels..];
            last_frame = match mode {
                OpusMode::CeltOnly => {
                    self.decode_celt_only_frame(frame, &ctx, &mut transition, out_frame)?
                }
                OpusMode::SilkOnly => {
                    self.decode_silk_only_frame(frame, &ctx, &mut transition, out_frame)?
                }
                OpusMode::Hybrid => self.decode_hybrid_frame(frame, &ctx, out_frame)?,
            };
            written_per_channel += last_frame.samples_per_channel;
        }

        self.prev_mode = Some(mode);
        self.last_had_redundancy = last_frame.had_redundancy;
        self.last_celt_to_silk = last_frame.celt_to_silk;
        self.prev_redundancy = last_frame.had_redundancy;
        self.loss_count = 0;
        self.last_packet_duration = written_per_channel;
        self.store_last_output(out, written_per_channel);
        Ok(written_per_channel)
    }

    /// Decide qué transiciones de modo aplica este paquete (`opus_decode_frame`): al entrar en
    /// CELT desde otro modo sin redundancia, la primera trama se funde con silencio tras reiniciar
    /// CELT; al salir de CELT, se reinicia SILK.
    fn packet_transition(&self, mode: OpusMode) -> PacketTransition {
        let transition = self.prev_mode.is_some()
            && ((mode == OpusMode::CeltOnly
                && self.prev_mode != Some(OpusMode::CeltOnly)
                && !self.prev_redundancy)
                || (mode != OpusMode::CeltOnly && self.prev_mode == Some(OpusMode::CeltOnly)));
        let into_celt = transition && mode == OpusMode::CeltOnly;
        PacketTransition {
            celt_transition: if into_celt {
                vec![0i16; self.samples_5ms() * self.channels as usize]
            } else {
                Vec::new()
            },
            apply_celt_transition: into_celt,
            reset_silk: transition && self.prev_mode == Some(OpusMode::CeltOnly),
        }
    }

    /// Decodifica una trama solo CELT y, si es la primera tras un cambio de modo, la funde con la
    /// transición.
    fn decode_celt_only_frame(
        &mut self,
        frame: &[u8],
        ctx: &PacketContext,
        transition: &mut PacketTransition,
        out_frame: &mut [i16],
    ) -> Result<FrameOutcome, Error> {
        let channels = self.channels as usize;
        if transition.apply_celt_transition {
            self.celt.reset();
        }
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
        if transition.apply_celt_transition {
            apply_transition_fade_i16(
                &transition.celt_transition,
                &mut out_frame[..celt_frame.samples_per_channel * channels],
                self.samples_2_5ms().min(celt_frame.samples_per_channel / 2),
                channels,
                self.celt.window(),
                self.fs_hz,
            );
            transition.apply_celt_transition = false;
        }
        self.last_packet_split_count += self.celt.last_split_count();
        self.last_final_range = self.celt.final_range();
        Ok(FrameOutcome {
            samples_per_channel: celt_frame.samples_per_channel,
            had_redundancy: false,
            celt_to_silk: false,
        })
    }

    /// Decodifica una trama solo SILK y la redundancia CELT que pueda llevar al final.
    fn decode_silk_only_frame(
        &mut self,
        frame: &[u8],
        ctx: &PacketContext,
        transition: &mut PacketTransition,
        out_frame: &mut [i16],
    ) -> Result<FrameOutcome, Error> {
        if transition.reset_silk {
            self.silk.reset();
            transition.reset_silk = false;
        }
        let silk_frame = self
            .silk
            .decode_frame(frame, ctx.silk_request(), out_frame)?;
        let mut redundancy_rng = 0u32;
        if silk_frame.consumed_redundancy {
            let redundancy_data = &frame[frame.len() - silk_frame.redundancy_bytes..];
            redundancy_rng =
                self.apply_silk_only_redundancy(redundancy_data, ctx, &silk_frame, out_frame)?;
        }
        self.last_final_range = self.silk.final_range() ^ redundancy_rng;
        Ok(FrameOutcome {
            samples_per_channel: silk_frame.samples_per_channel,
            had_redundancy: silk_frame.consumed_redundancy && !silk_frame.celt_to_silk,
            celt_to_silk: silk_frame.celt_to_silk,
        })
    }

    /// Decodifica la trama CELT redundante de 5 ms de una trama solo SILK y la funde con ella: al
    /// final si la transición es de SILK a CELT, al principio si es de CELT a SILK. Devuelve el
    /// `final_range` de la redundancia.
    fn apply_silk_only_redundancy(
        &mut self,
        redundancy_data: &[u8],
        ctx: &PacketContext,
        silk_frame: &silk::SilkFrameDecode,
        out_frame: &mut [i16],
    ) -> Result<u32, Error> {
        let channels = self.channels as usize;
        let overlap = self.samples_2_5ms();
        let mut redundancy_out = vec![0i16; self.samples_5ms() * channels];
        if !silk_frame.celt_to_silk {
            self.celt.reset();
        }
        self.celt.set_start_band(0);
        self.celt.set_end_band(packet::celt_end_band(ctx.config));
        let redundancy_frame = self.celt.decode_frame(
            redundancy_data,
            REDUNDANT_FRAME_SAMPLES_48K,
            ctx.config,
            ctx.packet_channels,
            &mut redundancy_out,
        )?;
        self.celt.clear_end_band();
        let redundancy_rng = self.celt.final_range();
        if !silk_frame.celt_to_silk {
            let silk_tail_start = (silk_frame.samples_per_channel - overlap) * channels;
            let silk_tail_end = silk_frame.samples_per_channel * channels;
            let redundancy_start = overlap * channels;
            let redundancy_end = redundancy_start + overlap * channels;
            let silk_tail = out_frame[silk_tail_start..silk_tail_end].to_vec();
            smooth_fade_i16(
                &silk_tail,
                &redundancy_out[redundancy_start..redundancy_end],
                &mut out_frame[silk_tail_start..silk_tail_end],
                overlap,
                channels,
                self.celt.window(),
                self.fs_hz,
            );
        } else {
            let frame_len = silk_frame.samples_per_channel * channels;
            apply_transition_fade_i16(
                &redundancy_out,
                &mut out_frame[..frame_len],
                overlap.min(redundancy_frame.samples_per_channel / 2),
                channels,
                self.celt.window(),
                self.fs_hz,
            );
        }
        Ok(redundancy_rng)
    }

    /// Decodifica una trama híbrida: SILK, la cabecera de redundancia, la redundancia CELT→SILK si
    /// la hay, la capa CELT desde la banda 17 y, por último, la redundancia SILK→CELT.
    fn decode_hybrid_frame(
        &mut self,
        frame: &[u8],
        ctx: &PacketContext,
        out_frame: &mut [i16],
    ) -> Result<FrameOutcome, Error> {
        let channels = self.channels as usize;
        let mut ec = EcDec::new(frame);
        let silk_samples =
            self.silk
                .decode_frame_with_ec(&mut ec, ctx.silk_request(), out_frame)?;
        let header = read_hybrid_redundancy(&mut ec, frame);
        let redundancy_data = &frame[frame.len() - header.bytes..];

        let mut celt_to_silk_audio = Vec::new();
        let mut celt_to_silk_samples = 0usize;
        let mut redundant_rng = 0u32;
        let apply_celt_to_silk_audio = header.present
            && header.celt_to_silk
            && (self.prev_mode != Some(OpusMode::SilkOnly) || self.prev_redundancy);
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

/// Transiciones de modo pendientes dentro de un paquete; solo afectan a su primera trama.
struct PacketTransition {
    /// Señal con que se funde la primera trama CELT tras el cambio de modo.
    celt_transition: Vec<i16>,
    /// La primera trama CELT aún debe reiniciar CELT y fundirse con `celt_transition`.
    apply_celt_transition: bool,
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

/// Cabecera de redundancia de una trama híbrida.
struct HybridRedundancy {
    present: bool,
    celt_to_silk: bool,
    /// Bytes de redundancia al final de la trama; 0 si no hay.
    bytes: usize,
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

/// Lee la cabecera de redundancia de una trama híbrida (`opus_decode_frame`) y recorta el
/// decodificador de rango para que la capa CELT no lea los bytes de la redundancia.
fn read_hybrid_redundancy(ec: &mut EcDec<'_>, frame: &[u8]) -> HybridRedundancy {
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
    HybridRedundancy {
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

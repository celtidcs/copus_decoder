//! Decodificador multiflujo: un `OpusDecoder` por flujo y el reparto de sus canales según el
//! mapa de canales (RFC 7845 §5.1.1).
//!
//! Sigue la lógica de `opus_multistream_decoder.c` de libopus.

use std::borrow::Cow;

use crate::packet;
use crate::{OpusDecoder, OpusError};

/// Decodificación de un flujo con un tipo de muestra: [`OpusDecoder::decode`] o
/// [`OpusDecoder::decode_float`].
type StreamDecode<T> = fn(&mut OpusDecoder, &[u8], &mut [T], bool) -> Result<usize, OpusError>;

/// Entrada del mapa de canales que deja el canal de salida en silencio (RFC 7845 §5.1.1).
const SILENT_CHANNEL: u8 = 255;
/// Máximo de canales de salida: el mapa usa un byte por canal (RFC 7845 §5.1.1).
const MAX_OUTPUT_CHANNELS: usize = 255;

/// Decodificador Opus multiflujo.
///
/// Tiene un `OpusDecoder` por flujo: estéreo para los flujos acoplados y mono para el resto.
pub struct OpusMultistreamDecoder {
    /// Un decodificador por flujo: los acoplados, estéreo; el resto, mono.
    decoders: Vec<OpusDecoder>,
    /// Canales de salida (de 1 a 255).
    nb_channels: usize,
    /// Flujos del paquete, acoplados y mono.
    nb_streams: usize,
    /// Flujos acoplados en estéreo, que son siempre los primeros.
    nb_coupled_streams: usize,
    /// `mapping[canal_de_salida]` es el índice del canal decodificado, contando primero los dos
    /// de cada flujo acoplado y después uno por flujo mono; `SILENT_CHANNEL` deja el canal en
    /// silencio.
    mapping: Vec<u8>,
    /// Frecuencia de salida, la misma para todos los flujos.
    sample_rate: u32,
}

impl OpusMultistreamDecoder {
    /// Crea un decodificador multiflujo con frecuencia de salida `sample_rate`, `nb_channels`
    /// canales de salida, `nb_streams` flujos (los `nb_coupled_streams` primeros, estéreo) y el
    /// mapa de canales `mapping`, con una entrada por canal de salida.
    ///
    /// # Errores
    ///
    /// `InvalidArgument`, con el nombre del parámetro, si no hay canales o hay más de 255, si no
    /// hay flujos, si hay más acoplados que flujos o más flujos de los que caben en el mapa, si
    /// `mapping` no tiene una entrada por canal o si alguna señala un canal que no existe; y los
    /// de [`OpusDecoder::new`] si la frecuencia no está admitida.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use copus_decoder::OpusMultistreamDecoder;
    ///
    /// let decoder = OpusMultistreamDecoder::new(48_000, 2, 2, 0, &[0, 1])?;
    /// # let _ = decoder;
    /// # Ok::<(), copus_decoder::OpusError>(())
    /// ```
    pub fn new(
        sample_rate: u32,
        nb_channels: usize,
        nb_streams: usize,
        nb_coupled_streams: usize,
        mapping: &[u8],
    ) -> Result<Self, OpusError> {
        if nb_channels == 0 || nb_channels > MAX_OUTPUT_CHANNELS {
            return Err(OpusError::InvalidArgument("nb_channels"));
        }
        if nb_streams == 0 {
            return Err(OpusError::InvalidArgument("nb_streams"));
        }
        if nb_coupled_streams > nb_streams {
            return Err(OpusError::InvalidArgument("nb_coupled_streams"));
        }
        if nb_streams > MAX_OUTPUT_CHANNELS.saturating_sub(nb_coupled_streams) {
            return Err(OpusError::InvalidArgument("nb_streams"));
        }
        if mapping.len() != nb_channels {
            return Err(OpusError::InvalidArgument("mapping"));
        }

        let total_decoded_channels = (2 * nb_coupled_streams) + (nb_streams - nb_coupled_streams);
        for &slot in mapping {
            if slot != SILENT_CHANNEL && usize::from(slot) >= total_decoded_channels {
                return Err(OpusError::InvalidArgument("mapping"));
            }
        }

        let mut decoders = Vec::with_capacity(nb_streams);
        for stream_idx in 0..nb_streams {
            let channels = if stream_idx < nb_coupled_streams {
                2
            } else {
                1
            };
            decoders.push(OpusDecoder::new(sample_rate, channels)?);
        }

        Ok(Self {
            decoders,
            nb_channels,
            nb_streams,
            nb_coupled_streams,
            mapping: mapping.to_vec(),
            sample_rate,
        })
    }

    /// Decodifica un paquete multiflujo en PCM `i16` entrelazado.
    ///
    /// `pcm` recibe las muestras de los `nb_channels` canales de salida, entrelazadas, en el
    /// orden del mapa de canales. Un paquete vacío oculta una pérdida en todos los flujos. Con
    /// `fec` a `true`, `packet` es el paquete que sigue a una pérdida y cada flujo la recupera con
    /// la corrección de errores de la banda (FEC) de su subpaquete, como
    /// [`OpusDecoder::decode`] con `fec` y como `opus_multistream_decode` de libopus: la salida dura
    /// lo que el último paquete y, si el subpaquete no trae con qué recuperarla (o dura más que la
    /// pérdida), ese flujo la oculta. Ahí libopus devolvería `OPUS_BUFFER_TOO_SMALL`, porque recibe
    /// la duración pedida; aquí se oculta, como en un solo flujo. Devuelve las muestras por
    /// canal.
    ///
    /// # Errores
    ///
    /// `InvalidPacket` si el paquete no se puede partir en un subpaquete por flujo o los flujos
    /// no duran lo mismo, `BufferTooSmall` si la trama no cabe en `pcm`, y los de
    /// [`OpusDecoder::decode`] en cada flujo.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use copus_decoder::OpusMultistreamDecoder;
    ///
    /// let mut decoder = OpusMultistreamDecoder::new(48_000, 2, 2, 0, &[0, 1])?;
    /// let packet = std::fs::read("multistream-frame.opus")?;
    /// let mut pcm = vec![0i16; 960 * 2];
    /// let samples = decoder.decode(&packet, &mut pcm, false)?;
    /// # let _ = samples;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn decode(
        &mut self,
        packet: &[u8],
        pcm: &mut [i16],
        fec: bool,
    ) -> Result<usize, OpusError> {
        self.decode_samples(packet, pcm, fec, OpusDecoder::decode)
    }

    /// Decodifica un paquete multiflujo en PCM `f32` entrelazado; por lo demás, como
    /// [`OpusMultistreamDecoder::decode`].
    ///
    /// En esta versión, la salida es la de [`OpusMultistreamDecoder::decode`] dividida entre
    /// 32 768: tiene la resolución de 16 bits, no la de la salida en coma flotante de libopus.
    ///
    /// # Errores
    ///
    /// Los de [`OpusMultistreamDecoder::decode`], con [`OpusDecoder::decode_float`] en cada
    /// flujo.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use copus_decoder::OpusMultistreamDecoder;
    ///
    /// let mut decoder = OpusMultistreamDecoder::new(48_000, 2, 2, 0, &[0, 1])?;
    /// let packet = std::fs::read("multistream-frame.opus")?;
    /// let mut pcm = vec![0.0f32; 960 * 2];
    /// let samples = decoder.decode_float(&packet, &mut pcm, false)?;
    /// # let _ = samples;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn decode_float(
        &mut self,
        packet: &[u8],
        pcm: &mut [f32],
        fec: bool,
    ) -> Result<usize, OpusError> {
        self.decode_samples(packet, pcm, fec, OpusDecoder::decode_float)
    }

    /// Camino común de [`Self::decode`] y [`Self::decode_float`]: parte el paquete, decodifica
    /// cada flujo con `decode_stream` y reparte sus canales según el mapa.
    fn decode_samples<T: Copy + Default>(
        &mut self,
        packet: &[u8],
        pcm: &mut [T],
        fec: bool,
        decode_stream: StreamDecode<T>,
    ) -> Result<usize, OpusError> {
        let sub_packets = if packet.is_empty() {
            vec![Cow::Borrowed(&[][..]); self.nb_streams]
        } else {
            split_multistream_packet(packet, self.nb_streams)?
        };
        let frame_size = self.expected_frame_size(&sub_packets, fec)?;
        let needed = frame_size * self.nb_channels;
        if pcm.len() < needed {
            return Err(OpusError::BufferTooSmall);
        }
        if frame_size == 0 {
            return Ok(0);
        }

        let mut stream_pcm = Vec::with_capacity(self.nb_streams);
        for (stream_idx, sub_packet) in sub_packets.iter().enumerate() {
            let channels = self.stream_channels(stream_idx);
            let mut buf = vec![T::default(); frame_size * channels];
            let written = decode_stream(&mut self.decoders[stream_idx], sub_packet, &mut buf, fec)?;
            if written != frame_size {
                return Err(OpusError::InvalidPacket);
            }
            stream_pcm.push(buf);
        }

        let output = Interleaved {
            stride: self.nb_channels,
            frame_size,
        };
        for (channel_idx, &slot) in self.mapping.iter().enumerate() {
            if slot == SILENT_CHANNEL {
                output.fill_channel(pcm, channel_idx, T::default());
                continue;
            }
            let (stream_idx, source_channel) = self.slot_to_stream_channel(usize::from(slot));
            let source = Interleaved {
                stride: self.stream_channels(stream_idx),
                frame_size,
            };
            output.copy_channel(
                pcm,
                channel_idx,
                &stream_pcm[stream_idx],
                source,
                source_channel,
            );
        }

        Ok(frame_size)
    }

    /// Reinicia los decodificadores de todos los flujos, como si no hubieran recibido nada.
    pub fn reset(&mut self) {
        for decoder in &mut self.decoders {
            decoder.reset();
        }
    }

    /// Muestras por canal que dará la llamada: las de sus subpaquetes, que han de coincidir, o
    /// las del último paquete si se oculta o se recupera con FEC una pérdida. Con FEC, los
    /// subpaquetes se validan igual, como en libopus.
    ///
    /// # Errores
    ///
    /// `InvalidPacket` si algún subpaquete no es válido o los subpaquetes duran distinto.
    fn expected_frame_size(
        &self,
        sub_packets: &[Cow<'_, [u8]>],
        fec: bool,
    ) -> Result<usize, OpusError> {
        match self.sub_packet_samples(sub_packets)? {
            Some(samples) if !fec => Ok(samples),
            _ => Ok(self
                .decoders
                .first()
                .map(OpusDecoder::last_packet_duration)
                .unwrap_or(0)),
        }
    }

    /// Muestras por canal de los subpaquetes no vacíos, que han de coincidir; `None` si todos
    /// están vacíos.
    ///
    /// # Errores
    ///
    /// `InvalidPacket` si algún subpaquete no es válido o los subpaquetes duran distinto.
    fn sub_packet_samples(
        &self,
        sub_packets: &[Cow<'_, [u8]>],
    ) -> Result<Option<usize>, OpusError> {
        let mut frame_size = None;
        for sub_packet in sub_packets.iter().filter(|packet| !packet.is_empty()) {
            let samples = packet::parse_packet(sub_packet)
                .map_err(OpusError::from)?
                .samples_per_channel(self.sample_rate);
            if let Some(expected) = frame_size {
                if expected != samples {
                    return Err(OpusError::InvalidPacket);
                }
            } else {
                frame_size = Some(samples);
            }
        }

        Ok(frame_size)
    }

    /// Canales del flujo `stream_idx`: 2 si es acoplado y 1 si es mono.
    fn stream_channels(&self, stream_idx: usize) -> usize {
        if stream_idx < self.nb_coupled_streams {
            2
        } else {
            1
        }
    }

    /// Flujo, y canal dentro del flujo, que corresponden a la entrada `slot` del mapa.
    fn slot_to_stream_channel(&self, slot: usize) -> (usize, usize) {
        let coupled_slots = 2 * self.nb_coupled_streams;
        if slot < coupled_slots {
            (slot / 2, slot % 2)
        } else {
            (self.nb_coupled_streams + (slot - coupled_slots), 0)
        }
    }
}

/// Parte un paquete multiflujo en un subpaquete por flujo, en orden, como
/// `opus_multistream_decode_native` de libopus: todos menos el último van autodelimitados
/// (RFC 6716, apéndice B; [`packet::split_self_delimited`]) y se devuelven en el formato normal;
/// el último ocupa el resto. Un paquete vacío da un subpaquete vacío (ocultación) por flujo.
///
/// # Errores
///
/// `InvalidArgument` si no hay flujos, e `InvalidPacket` si el paquete es más corto que
/// `2 * nb_streams - 1` bytes (el mínimo de libopus), si un subpaquete autodelimitado no es
/// válido o se sale del paquete, o si no queda nada para el último flujo.
fn split_multistream_packet(
    packet: &[u8],
    nb_streams: usize,
) -> Result<Vec<Cow<'_, [u8]>>, OpusError> {
    if nb_streams == 0 {
        return Err(OpusError::InvalidArgument("nb_streams"));
    }
    if packet.is_empty() {
        return Ok(vec![Cow::Borrowed(packet); nb_streams]);
    }
    if packet.len() < 2 * nb_streams - 1 {
        return Err(OpusError::InvalidPacket);
    }

    let mut out = Vec::with_capacity(nb_streams);
    let mut rest = packet;
    for _stream_idx in 1..nb_streams {
        let (sub_packet, used) = packet::split_self_delimited(rest).map_err(OpusError::from)?;
        out.push(Cow::Owned(sub_packet));
        rest = &rest[used..];
    }
    if rest.is_empty() {
        return Err(OpusError::InvalidPacket);
    }
    out.push(Cow::Borrowed(rest));
    Ok(out)
}

/// Forma de un búfer entrelazado: `stride` canales y `frame_size` muestras por canal.
#[derive(Debug, Clone, Copy)]
struct Interleaved {
    stride: usize,
    frame_size: usize,
}

impl Interleaved {
    /// Escribe `value` en todas las muestras del canal `channel` de `pcm`.
    fn fill_channel<T: Copy>(self, pcm: &mut [T], channel: usize, value: T) {
        for sample_idx in 0..self.frame_size {
            pcm[sample_idx * self.stride + channel] = value;
        }
    }

    /// Copia en el canal `channel` de `pcm` el canal `src_channel` de `src`, que tiene la forma
    /// `src_shape`.
    fn copy_channel<T: Copy>(
        self,
        pcm: &mut [T],
        channel: usize,
        src: &[T],
        src_shape: Interleaved,
        src_channel: usize,
    ) {
        for sample_idx in 0..self.frame_size {
            pcm[sample_idx * self.stride + channel] =
                src[sample_idx * src_shape.stride + src_channel];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paquete de dos flujos en el formato de libopus: el primero, `[0x08, 10, 11]` (código 0),
    /// lleva la longitud de su trama (2) tras el TOC; el segundo, `[0x08, 20]`, va normal.
    #[test]
    fn split_two_stream_packet_in_libopus_format() {
        let packet = [0x08, 2, 10, 11, 0x08, 20];
        let sub_packets = split_multistream_packet(&packet, 2).unwrap();
        assert_eq!(sub_packets, vec![&[0x08, 10, 11][..], &[0x08, 20][..]]);
    }

    /// Con relleno (código 3, una trama): la longitud autodelimitada va tras el byte de cuenta y
    /// la longitud del relleno, y el relleno sigue a la trama y pertenece al primer flujo.
    #[test]
    fn split_self_delimited_sub_packet_with_padding() {
        let first = [0x0B, 0x41, 2, 7, 8, 0, 0];
        let packet = [0x0B, 0x41, 2, 2, 7, 8, 0, 0, 0x08, 20];
        let sub_packets = split_multistream_packet(&packet, 2).unwrap();
        assert_eq!(sub_packets, vec![&first[..], &[0x08, 20][..]]);
    }

    /// Sin bytes para el último flujo, o más corto que `2 * nb_streams - 1`, no es válido.
    #[test]
    fn split_rejects_missing_last_stream() {
        assert!(split_multistream_packet(&[0x08, 0, 0x08], 3).is_err());
        assert!(split_multistream_packet(&[0x08, 1, 10], 2).is_err());
    }

    /// Un paquete vacío da un subpaquete vacío (ocultación) por flujo.
    #[test]
    fn split_empty_packet_for_plc() {
        let sub_packets = split_multistream_packet(&[], 3).unwrap();
        assert_eq!(sub_packets, vec![&[][..], &[][..], &[][..]]);
    }
}

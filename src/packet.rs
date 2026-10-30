use crate::Error;

pub(crate) const MAX_FRAMES: usize = 48;
pub(crate) const MAX_DURATION_SAMPLES_48K: usize = 5760; // 120 ms @ 48 kHz
/// Bytes más largos que puede tener una trama (RFC 6716 §3.2.1). Una longitud explícita no puede
/// pasar de ahí (`255 + 4 · 255`); la implícita de la última trama, sí, y entonces el paquete no
/// es válido (regla R2 de RFC 6716 §3.4; `opus_packet_parse_impl` de libopus).
const MAX_FRAME_BYTES: usize = 1275;

/// Frecuencia de referencia de Opus: las duraciones del TOC se cuentan a 48 kHz (RFC 6716 §2).
pub(crate) const OPUS_RATE_HZ: usize = 48_000;
/// Bits por byte, para comparar longitudes en bytes con el contador de bits del decodificador de
/// rango.
pub(crate) const BITS_PER_BYTE: usize = 8;

/// Posición de la configuración en el TOC: sus 5 bits altos (RFC 6716 §3.1).
const TOC_CONFIG_SHIFT: u8 = 3;
/// Bit `s` del TOC: el paquete es estéreo (RFC 6716 §3.1).
const TOC_STEREO_FLAG: u8 = 0x04;
/// Bits `c` del TOC: código de número de tramas, de 0 a 3 (RFC 6716 §3.1).
const TOC_FRAME_CODE_MASK: u8 = 0x03;
/// Última configuración solo SILK (RFC 6716 §3.1, Tabla 2).
pub(crate) const LAST_SILK_ONLY_CONFIG: u8 = 11;
/// Última configuración híbrida (RFC 6716 §3.1, Tabla 2); de la 16 a la 31 son solo CELT.
pub(crate) const LAST_HYBRID_CONFIG: u8 = 15;
/// Duraciones de trama de un mismo ancho de banda en los modos SILK y CELT: cada grupo de 4
/// configuraciones consecutivas recorre estas duraciones (RFC 6716 §3.1, Tabla 2).
pub(crate) const DURATIONS_PER_BANDWIDTH: u8 = 4;
/// Duraciones de trama solo SILK a 48 kHz: 10, 20, 40 y 60 ms.
pub(crate) const SILK_FRAME_SAMPLES_48K: [usize; 4] = [480, 960, 1_920, 2_880];
/// Duraciones de trama híbrida a 48 kHz: 10 y 20 ms (configuración par e impar).
const HYBRID_FRAME_SAMPLES_48K: [usize; 2] = [480, 960];
/// Duraciones de trama solo CELT a 48 kHz: 2,5, 5, 10 y 20 ms. La posición es el LM de CELT.
pub(crate) const CELT_FRAME_SAMPLES_48K: [usize; 4] = [120, 240, 480, 960];

/// Última configuración solo SILK de banda estrecha y última híbrida de banda superancha
/// (RFC 6716 §3.1, Tabla 2).
const LAST_NARROWBAND_SILK_CONFIG: u8 = 3;
const LAST_SUPERWIDEBAND_HYBRID_CONFIG: u8 = 13;
/// Banda final de CELT para cada ancho de banda (`endband` en `opus_decode_frame`): estrecha 13,
/// media y ancha 17, superancha 19 y completa 21.
const END_BAND_NARROWBAND: usize = 13;
const END_BAND_WIDEBAND: usize = 17;
const END_BAND_SUPERWIDEBAND: usize = 19;
const END_BAND_FULLBAND: usize = 21;
/// Banda final de las configuraciones solo CELT, por ancho de banda: cada grupo de
/// `DURATIONS_PER_BANDWIDTH` configuraciones desde la 16 (RFC 6716 §3.1, Tabla 2).
const CELT_ONLY_END_BANDS: [usize; 4] = [
    END_BAND_NARROWBAND,
    END_BAND_WIDEBAND,
    END_BAND_SUPERWIDEBAND,
    END_BAND_FULLBAND,
];

/// Bits del byte de número de tramas del código 3 (RFC 6716 §3.2.5): número de tramas (M),
/// relleno (p) y tasa variable (v).
const CODE3_FRAME_COUNT_MASK: u8 = 0x3f;
const CODE3_PADDING_FLAG: u8 = 0x40;
const CODE3_VBR_FLAG: u8 = 0x80;
/// Un primer byte de longitud de trama menor que este valor es la longitud entera; desde él, la
/// longitud ocupa dos bytes (RFC 6716 §3.2.1).
const TWO_BYTE_SIZE_THRESHOLD: usize = 252;
/// Peso del segundo byte de una longitud de dos bytes: `primero + 4 * segundo` (RFC 6716 §3.2.1).
const SECOND_SIZE_BYTE_WEIGHT: usize = 4;
/// Un byte de relleno con este valor aporta `PADDING_CONTINUE_BYTES` y sigue otro byte
/// (RFC 6716 §3.2.5).
const PADDING_CONTINUE: usize = 255;
const PADDING_CONTINUE_BYTES: usize = 254;

/// Configuración (0 a 31) del TOC.
pub(crate) fn toc_config(toc: u8) -> u8 {
    toc >> TOC_CONFIG_SHIFT
}

/// Paquete Opus partido en tramas (RFC 6716 §3), sin copiar sus bytes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ParsedPacket<'a> {
    /// Byte TOC, del que salen la configuración y el modo.
    pub toc: u8,
    /// Canales codificados (bit `s` del TOC).
    pub packet_channels: u8,
    /// Tramas del paquete, de 1 a `MAX_FRAMES`.
    pub frame_count: usize,
    /// Las `frame_count` primeras son las tramas; el resto, vacías.
    pub frames: [&'a [u8]; MAX_FRAMES],
    /// Duración de cada trama, en muestras a 48 kHz.
    pub samples_per_frame_48k: usize,
}

impl<'a> ParsedPacket<'a> {
    /// Las tramas del paquete.
    pub fn frames(&self) -> &[&'a [u8]] {
        &self.frames[..self.frame_count]
    }

    /// Duración del paquete, en muestras por canal a 48 kHz.
    pub fn samples_per_channel_48k(&self) -> usize {
        self.samples_per_frame_48k * self.frame_count
    }

    /// Duración del paquete, en muestras por canal a `fs_hz`.
    pub fn samples_per_channel(&self, fs_hz: u32) -> usize {
        (self.samples_per_channel_48k() * fs_hz as usize) / OPUS_RATE_HZ
    }
}

/// Parte `packet` en tramas según su TOC (RFC 6716 §3.2), como `opus_packet_parse_impl` de
/// libopus. La longitud del paquete no tiene límite: solo lo tienen sus tramas (1275 bytes) y su
/// duración (120 ms); el relleno del código 3 puede ser tan largo como se quiera.
///
/// # Errores
///
/// `BadPacket` si el paquete incumple alguna de las reglas R1–R7 de RFC 6716 §3.4.
pub(crate) fn parse_packet(packet: &[u8]) -> Result<ParsedPacket<'_>, Error> {
    if packet.is_empty() {
        return Err(Error::BadPacket);
    }

    let toc = packet[0];
    let packet_channels = if (toc & TOC_STEREO_FLAG) != 0 { 2 } else { 1 };
    let samples_per_frame_48k = samples_per_frame_48k(toc);

    let mut frames: [&[u8]; MAX_FRAMES] = [&[]; MAX_FRAMES];
    let payload = &packet[1..];
    let frame_count = match toc & TOC_FRAME_CODE_MASK {
        0 => {
            frames[0] = payload;
            1
        }
        1 => split_code1(payload, &mut frames)?,
        2 => split_code2(payload, &mut frames)?,
        3 => split_code3(payload, &mut frames)?,
        _ => return Err(Error::BadPacket),
    };

    // Las tramas implícitas son la última o, si la tasa es constante, todas iguales a ella.
    if frames[frame_count - 1].len() > MAX_FRAME_BYTES {
        return Err(Error::BadPacket);
    }

    let total_samples_48k = samples_per_frame_48k.saturating_mul(frame_count);
    if total_samples_48k > MAX_DURATION_SAMPLES_48K {
        return Err(Error::BadPacket);
    }

    Ok(ParsedPacket {
        toc,
        packet_channels,
        frame_count,
        frames,
        samples_per_frame_48k,
    })
}

/// Código 1: dos tramas del mismo tamaño, que se reparten la carga a partes iguales. Con la carga
/// vacía son dos tramas de 0 bytes, como en libopus (`opus_packet_parse_impl` solo exige una
/// longitud par), que se ocultan como las de la DTX.
fn split_code1<'a>(payload: &'a [u8], frames: &mut [&'a [u8]; MAX_FRAMES]) -> Result<usize, Error> {
    if payload.len() % 2 != 0 {
        return Err(Error::BadPacket);
    }
    let (first, second) = payload.split_at(payload.len() / 2);
    frames[0] = first;
    frames[1] = second;
    Ok(2)
}

/// Código 2: dos tramas; la longitud de la primera va delante y la segunda ocupa el resto.
fn split_code2<'a>(payload: &'a [u8], frames: &mut [&'a [u8]; MAX_FRAMES]) -> Result<usize, Error> {
    let (sz0, used) = parse_size(payload)?;
    let payload = &payload[used..];
    if sz0 > payload.len() {
        return Err(Error::BadPacket);
    }
    let (first, second) = payload.split_at(sz0);
    frames[0] = first;
    frames[1] = second;
    Ok(2)
}

/// Código 3: un byte con el número de tramas y las banderas de relleno y de tasa variable, el
/// relleno opcional, las longitudes (si la tasa es variable) y las tramas.
fn split_code3<'a>(payload: &'a [u8], frames: &mut [&'a [u8]; MAX_FRAMES]) -> Result<usize, Error> {
    let Some(&header) = payload.first() else {
        return Err(Error::BadPacket);
    };
    let frame_count = (header & CODE3_FRAME_COUNT_MASK) as usize;
    if !(1..=MAX_FRAMES).contains(&frame_count) {
        return Err(Error::BadPacket);
    }
    let (start, data_end) = code3_data_bounds(payload, (header & CODE3_PADDING_FLAG) != 0)?;
    let (start, sizes) = if (header & CODE3_VBR_FLAG) != 0 {
        code3_vbr_sizes(payload, start, data_end, frame_count)?
    } else {
        (
            start,
            code3_cbr_sizes(data_end.saturating_sub(start), frame_count)?,
        )
    };

    let mut off = start;
    for (frame, &sz) in frames.iter_mut().zip(&sizes).take(frame_count) {
        if off + sz > data_end {
            return Err(Error::BadPacket);
        }
        *frame = &payload[off..off + sz];
        off += sz;
    }
    if off != data_end {
        return Err(Error::BadPacket);
    }
    Ok(frame_count)
}

/// Primer byte tras la cabecera del código 3 (y tras la longitud del relleno, si lo hay) y fin
/// de los datos, antes del relleno.
fn code3_data_bounds(payload: &[u8], has_padding: bool) -> Result<(usize, usize), Error> {
    let mut start = 1usize;
    if !has_padding {
        return Ok((start, payload.len()));
    }
    let (pad_len, used) = parse_padding_len(&payload[start..])?;
    start += used;
    if pad_len > payload.len().saturating_sub(start) {
        return Err(Error::BadPacket);
    }
    let data_end = payload.len() - pad_len;
    if start > data_end {
        return Err(Error::BadPacket);
    }
    Ok((start, data_end))
}

/// Longitudes de un código 3 de tasa variable: van todas menos la última, que ocupa lo que
/// queda. Devuelve también dónde empiezan las tramas.
fn code3_vbr_sizes(
    payload: &[u8],
    mut start: usize,
    data_end: usize,
    frame_count: usize,
) -> Result<(usize, [usize; MAX_FRAMES]), Error> {
    let mut sizes = [0usize; MAX_FRAMES];
    let mut sum = 0usize;
    for size in sizes.iter_mut().take(frame_count - 1) {
        let (sz, used) = parse_size(&payload[start..data_end])?;
        start += used;
        *size = sz;
        sum = sum.saturating_add(sz);
    }
    let remaining = data_end.saturating_sub(start);
    if sum > remaining {
        return Err(Error::BadPacket);
    }
    sizes[frame_count - 1] = remaining - sum;
    Ok((start, sizes))
}

/// Longitudes de un código 3 de tasa constante: `remaining` bytes a partes iguales.
fn code3_cbr_sizes(remaining: usize, frame_count: usize) -> Result<[usize; MAX_FRAMES], Error> {
    if remaining % frame_count != 0 {
        return Err(Error::BadPacket);
    }
    let mut sizes = [0usize; MAX_FRAMES];
    sizes[..frame_count].fill(remaining / frame_count);
    Ok(sizes)
}

/// Forma de la cabecera de un subpaquete autodelimitado, hasta la longitud que añade la
/// autodelimitación.
struct SelfDelimitedLayout {
    /// Bytes de TOC, byte de cuenta, longitud del relleno y longitudes de trama explícitas: la
    /// longitud autodelimitada va justo después.
    header_end: usize,
    /// Bytes de las tramas cuya longitud ya figura en la cabecera.
    explicit_frames: usize,
    /// Tramas que miden lo que dice la longitud autodelimitada: 1, o todas si la tasa es
    /// constante (código 1 y código 3 sin la bandera de tasa variable).
    implicit_frames: usize,
    /// Bytes de relleno tras las tramas.
    padding: usize,
}

/// Separa el subpaquete autodelimitado (RFC 6716, apéndice B) del principio de `data`, como
/// `opus_packet_parse_impl` de libopus con `self_delimited` (`src/opus.c`, v1.6.1): tras el TOC,
/// el byte de cuenta, el relleno y las longitudes explícitas va la longitud de la última trama
/// (de cada trama, si la tasa es constante).
///
/// Devuelve el subpaquete en el formato normal, sin esa longitud, y los bytes de `data` que
/// ocupa, relleno incluido.
///
/// # Errores
///
/// `BadPacket` si la cabecera está truncada, las longitudes se salen de `data` o el subpaquete
/// no es un paquete válido.
pub(crate) fn split_self_delimited(data: &[u8]) -> Result<(Vec<u8>, usize), Error> {
    let layout = self_delimited_layout(data)?;
    let (last_size, used) = parse_size(&data[layout.header_end..])?;
    let frames_start = layout.header_end + used;
    let end = last_size
        .checked_mul(layout.implicit_frames)
        .and_then(|bytes| bytes.checked_add(layout.explicit_frames))
        .and_then(|bytes| bytes.checked_add(layout.padding))
        .and_then(|bytes| bytes.checked_add(frames_start))
        .filter(|&end| end <= data.len())
        .ok_or(Error::BadPacket)?;

    let mut packet = Vec::with_capacity(end - used);
    packet.extend_from_slice(&data[..layout.header_end]);
    packet.extend_from_slice(&data[frames_start..end]);
    parse_packet(&packet)?;
    Ok((packet, end))
}

/// Cabecera de un subpaquete autodelimitado según el código de tramas del TOC.
fn self_delimited_layout(data: &[u8]) -> Result<SelfDelimitedLayout, Error> {
    let Some(&toc) = data.first() else {
        return Err(Error::BadPacket);
    };
    let single = |header_end, explicit_frames, implicit_frames| SelfDelimitedLayout {
        header_end,
        explicit_frames,
        implicit_frames,
        padding: 0,
    };
    match toc & TOC_FRAME_CODE_MASK {
        0 => Ok(single(1, 0, 1)),
        1 => Ok(single(1, 0, 2)),
        2 => {
            let (first, used) = parse_size(&data[1..])?;
            Ok(single(1 + used, first, 1))
        }
        _ => code3_self_delimited_layout(data),
    }
}

/// Cabecera autodelimitada del código 3: byte de cuenta, relleno opcional y, si la tasa es
/// variable, las longitudes de todas las tramas menos la última.
fn code3_self_delimited_layout(data: &[u8]) -> Result<SelfDelimitedLayout, Error> {
    let Some(&header) = data.get(1) else {
        return Err(Error::BadPacket);
    };
    let frame_count = (header & CODE3_FRAME_COUNT_MASK) as usize;
    if !(1..=MAX_FRAMES).contains(&frame_count) {
        return Err(Error::BadPacket);
    }
    let mut header_end = 2;
    let mut padding = 0;
    if (header & CODE3_PADDING_FLAG) != 0 {
        let (pad_len, used) = parse_padding_len(&data[header_end..])?;
        header_end += used;
        padding = pad_len;
    }
    if (header & CODE3_VBR_FLAG) == 0 {
        return Ok(SelfDelimitedLayout {
            header_end,
            explicit_frames: 0,
            implicit_frames: frame_count,
            padding,
        });
    }
    let mut explicit_frames = 0usize;
    for _ in 1..frame_count {
        let (size, used) = parse_size(&data[header_end..])?;
        header_end += used;
        explicit_frames = explicit_frames.saturating_add(size);
    }
    Ok(SelfDelimitedLayout {
        header_end,
        explicit_frames,
        implicit_frames: 1,
        padding,
    })
}

/// Lee una longitud de trama de uno o dos bytes (RFC 6716 §3.2.1). La usan también los
/// subpaquetes autodelimitados del multiflujo.
///
/// Devuelve `(longitud, bytes_leídos)`, o `BadPacket` si faltan bytes.
fn parse_size(data: &[u8]) -> Result<(usize, usize), Error> {
    if data.is_empty() {
        return Err(Error::BadPacket);
    }
    let b0 = data[0] as usize;
    if b0 < TWO_BYTE_SIZE_THRESHOLD {
        Ok((b0, 1))
    } else {
        if data.len() < 2 {
            return Err(Error::BadPacket);
        }
        let b1 = data[1] as usize;
        Ok((SECOND_SIZE_BYTE_WEIGHT * b1 + b0, 2))
    }
}

fn parse_padding_len(data: &[u8]) -> Result<(usize, usize), Error> {
    let mut pad = 0usize;
    let mut used = 0usize;
    loop {
        if used >= data.len() {
            return Err(Error::BadPacket);
        }
        let p = data[used] as usize;
        used += 1;
        let add = if p == PADDING_CONTINUE {
            PADDING_CONTINUE_BYTES
        } else {
            p
        };
        pad = pad.saturating_add(add);
        if p != PADDING_CONTINUE {
            break;
        }
    }
    Ok((pad, used))
}

/// Muestras por trama a 48 kHz que indica el TOC (`opus_packet_get_samples_per_frame` de libopus
/// a 48 kHz).
fn samples_per_frame_48k(toc: u8) -> usize {
    let config = toc_config(toc);
    let duration_index = usize::from(config % DURATIONS_PER_BANDWIDTH);
    if config <= LAST_SILK_ONLY_CONFIG {
        SILK_FRAME_SAMPLES_48K[duration_index]
    } else if config <= LAST_HYBRID_CONFIG {
        HYBRID_FRAME_SAMPLES_48K[usize::from(config) % HYBRID_FRAME_SAMPLES_48K.len()]
    } else {
        CELT_FRAME_SAMPLES_48K[duration_index]
    }
}

/// Banda final (exclusiva) de CELT para la configuración del TOC: la del ancho de banda del
/// paquete, como `endband` en `opus_decode_frame` de libopus.
///
/// En solo SILK es la banda de la trama de redundancia; la banda media comparte la de la ancha.
/// Una configuración fuera de `0..=31` (no la produce `toc_config`) devuelve la banda completa.
pub(crate) fn celt_end_band(config: u8) -> usize {
    if config <= LAST_NARROWBAND_SILK_CONFIG {
        END_BAND_NARROWBAND
    } else if config <= LAST_SILK_ONLY_CONFIG {
        END_BAND_WIDEBAND
    } else if config <= LAST_SUPERWIDEBAND_HYBRID_CONFIG {
        END_BAND_SUPERWIDEBAND
    } else if config <= LAST_HYBRID_CONFIG {
        END_BAND_FULLBAND
    } else {
        let bandwidth = usize::from((config - LAST_HYBRID_CONFIG - 1) / DURATIONS_PER_BANDWIDTH);
        CELT_ONLY_END_BANDS
            .get(bandwidth)
            .copied()
            .unwrap_or(END_BAND_FULLBAND)
    }
}

/// Duración de una trama interna de SILK a 48 kHz (20 ms): las tramas SILK de 40 y 60 ms llevan
/// 2 y 3 tramas internas, y las de 10 y 20 ms, una (RFC 6716 §4.2.2).
const SILK_INTERNAL_FRAME_SAMPLES_48K: usize = 960;
/// Posición del bit más alto de un byte, desde la que se cuentan los bits de la cabecera SILK.
const HIGHEST_BIT: usize = BITS_PER_BYTE - 1;

/// Si la primera trama del paquete trae tramas LBRR (FEC en banda), como `opus_packet_has_lbrr`
/// de libopus (`src/opus_decoder.c`). Los paquetes solo CELT y los de primera trama vacía no las
/// traen.
///
/// La cabecera SILK empieza, para cada canal codificado, con un bit de actividad de voz por trama
/// interna y el indicador LBRR (RFC 6716 §4.2.3, §4.2.4). Como libopus, los indicadores se leen
/// directamente de los bits altos del primer byte: el del canal medio va tras sus `n` bits de
/// actividad, y el del lateral, tras `2n + 1` bits.
///
/// # Errores
///
/// Los de [`parse_packet`].
pub(crate) fn has_lbrr(packet: &[u8]) -> Result<bool, Error> {
    let parsed = parse_packet(packet)?;
    if toc_config(parsed.toc) > LAST_HYBRID_CONFIG {
        return Ok(false);
    }
    let Some(&first_byte) = parsed.frames()[0].first() else {
        return Ok(false);
    };
    let internal_frames = (parsed.samples_per_frame_48k / SILK_INTERNAL_FRAME_SAMPLES_48K).max(1);
    let bit = |position: usize| (first_byte >> (HIGHEST_BIT - position)) & 1 == 1;
    let mid_lbrr = bit(internal_frames);
    let side_lbrr = parsed.packet_channels == 2 && bit(2 * internal_frames + 1);
    Ok(mid_lbrr || side_lbrr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_vectors::{read_opus_demo_records, vectors_dir};
    use std::fs;
    use std::path::PathBuf;

    /// Versión de `samples_per_frame_48k` anterior a P3, por bits del TOC; sirve de oráculo.
    /// Versión anterior de la banda final de la redundancia SILK (`lib.rs`, antes de P3b).
    fn banda_final_redundancia_silk_anterior(config: u8) -> usize {
        match config {
            0..=3 => 13,
            4..=11 => 17,
            12..=13 => 19,
            _ => 21,
        }
    }

    /// Versión anterior de la banda final de CELT e híbrido (`celt::bandwidth_end`, antes de P3b).
    fn banda_final_celt_anterior(config: u8) -> usize {
        match config {
            12 | 13 => 19,
            14 | 15 => 21,
            16..=19 => 13,
            20..=23 => 17,
            24..=27 => 19,
            28..=31 => 21,
            _ => 21,
        }
    }

    /// La banda final unificada coincide con la función que la calculaba antes en cada dominio:
    /// la de SILK en sus configuraciones (0–11) y la de CELT en el resto.
    #[test]
    fn celt_end_band_coincide_con_las_versiones_anteriores() {
        for config in 0..=u8::MAX {
            let anterior = if config <= LAST_SILK_ONLY_CONFIG {
                banda_final_redundancia_silk_anterior(config)
            } else {
                banda_final_celt_anterior(config)
            };
            assert_eq!(celt_end_band(config), anterior, "config {config}");
        }
    }

    fn samples_per_frame_48k_por_bits(toc: u8) -> usize {
        if (toc & 0x80) != 0 {
            let audiosize = ((toc >> 3) & 0x03) as usize;
            (48_000usize << audiosize) / 400
        } else if (toc & 0x60) == 0x60 {
            if (toc & 0x08) != 0 {
                48_000usize / 50
            } else {
                48_000usize / 100
            }
        } else {
            let audiosize = ((toc >> 3) & 0x03) as usize;
            if audiosize == 3 {
                (48_000usize * 60) / 1000
            } else {
                (48_000usize << audiosize) / 100
            }
        }
    }

    #[test]
    fn la_duracion_por_configuracion_coincide_con_la_de_bits_en_los_256_toc() {
        for toc in 0..=u8::MAX {
            assert_eq!(
                samples_per_frame_48k(toc),
                samples_per_frame_48k_por_bits(toc),
                "TOC {toc:#04x}"
            );
        }
    }

    /// `opus_packet_has_lbrr` de libopus (`src/opus_decoder.c`, v1.6.1) con sus mismas
    /// operaciones, para una trama de código 0 cuyo primer byte es `first`; sirve de oráculo.
    fn libopus_has_lbrr(toc: u8, first: u8) -> bool {
        let config = toc >> 3;
        if config >= 16 {
            return false;
        }
        // `opus_packet_get_samples_per_frame(packet, 48000)`, de la Tabla 2 de RFC 6716.
        let packet_frame_size = if config >= 12 {
            if config & 1 == 1 { 960 } else { 480 }
        } else {
            [480, 960, 1920, 2880][usize::from(config & 3)]
        };
        let nb_frames = if packet_frame_size > 960 {
            packet_frame_size / 960
        } else {
            1
        };
        let mut lbrr = (first >> (7 - nb_frames)) & 1 == 1;
        if toc & 4 != 0 {
            lbrr = lbrr || (first >> (6 - 2 * nb_frames)) & 1 == 1;
        }
        lbrr
    }

    #[test]
    fn has_lbrr_matches_libopus_for_every_toc_and_first_byte() {
        for toc in (0..=u8::MAX).filter(|toc| toc & 3 == 0) {
            for first in 0..=u8::MAX {
                assert_eq!(
                    has_lbrr(&[toc, first, 0]).unwrap(),
                    libopus_has_lbrr(toc, first),
                    "toc {toc:#04x}, primer byte {first:#010b}"
                );
            }
        }
    }

    #[test]
    fn has_lbrr_is_false_for_an_empty_first_frame() {
        // Configuración 1 (SILK NB, 20 ms), mono, código 0, sin carga.
        assert!(!has_lbrr(&[1 << 3]).unwrap());
    }

    #[test]
    fn parse_code0_single_frame() {
        let packet = [0b0000_0000u8, 1, 2, 3, 4];
        let pp = parse_packet(&packet).unwrap();
        assert_eq!(pp.frame_count, 1);
        assert_eq!(pp.frames()[0], &[1, 2, 3, 4]);
    }

    #[test]
    fn parse_code1_two_frames_cbr() {
        let packet = [0b0000_0001u8, 1, 2, 3, 4];
        let pp = parse_packet(&packet).unwrap();
        assert_eq!(pp.frame_count, 2);
        assert_eq!(pp.frames()[0], &[1, 2]);
        assert_eq!(pp.frames()[1], &[3, 4]);
    }

    #[test]
    fn parse_code1_odd_payload_is_rejected() {
        let packet = [0b0000_0001u8, 1, 2, 3];
        let err = parse_packet(&packet).unwrap_err();
        assert!(matches!(err, Error::BadPacket));
    }

    #[test]
    fn parse_code2_two_frames_vbr_short_size() {
        // Una longitud menor que 252 ocupa un byte.
        let packet = [0b0000_0010u8, 2, 0xaa, 0xbb, 0xcc];
        let pp = parse_packet(&packet).unwrap();
        assert_eq!(pp.frame_count, 2);
        assert_eq!(pp.frames()[0], &[0xaa, 0xbb]);
        assert_eq!(pp.frames()[1], &[0xcc]);
    }

    #[test]
    fn parse_code2_two_frames_vbr_long_size() {
        // size field >=252 => two bytes; size = 4*b1 + b0.
        // Pick b0=252, b1=1 => size=256.
        let mut packet = Vec::<u8>::new();
        packet.push(0b0000_0010u8);
        packet.push(252);
        packet.push(1);
        packet.extend(std::iter::repeat_n(0x11, 256));
        packet.extend(std::iter::repeat_n(0x22, 10));

        let pp = parse_packet(&packet).unwrap();
        assert_eq!(pp.frame_count, 2);
        assert_eq!(pp.frames()[0].len(), 256);
        assert_eq!(pp.frames()[1].len(), 10);
        assert!(pp.frames()[0].iter().all(|&b| b == 0x11));
        assert!(pp.frames()[1].iter().all(|&b| b == 0x22));
    }

    #[test]
    fn parse_code3_cbr_n_frames() {
        // code=3, count=3, CBR, no padding
        let mut packet = Vec::<u8>::new();
        packet.push(0b0000_0011u8);
        packet.push(0b0000_0011u8); // 3 frames
        packet.extend([1, 2, 3, 4, 5, 6]); // 6 bytes => 2 bytes per frame

        let pp = parse_packet(&packet).unwrap();
        assert_eq!(pp.frame_count, 3);
        assert_eq!(pp.frames()[0], &[1, 2]);
        assert_eq!(pp.frames()[1], &[3, 4]);
        assert_eq!(pp.frames()[2], &[5, 6]);
    }

    #[test]
    fn parse_code3_vbr_n_frames() {
        // code=3, count=3, VBR, no padding
        // Longitudes 1 y 2; la última trama ocupa el resto (3).
        let mut packet = vec![
            0b0000_0011u8,
            0b1000_0011u8, // VBR + 3 frames
            1,             // size0
            2,             // size1
        ];
        packet.extend([0xa0]); // frame0 (1)
        packet.extend([0xb0, 0xb1]); // frame1 (2)
        packet.extend([0xc0, 0xc1, 0xc2]); // frame2 (3)

        let pp = parse_packet(&packet).unwrap();
        assert_eq!(pp.frame_count, 3);
        assert_eq!(pp.frames()[0], &[0xa0]);
        assert_eq!(pp.frames()[1], &[0xb0, 0xb1]);
        assert_eq!(pp.frames()[2], &[0xc0, 0xc1, 0xc2]);
    }

    #[test]
    fn parse_code3_padding() {
        // Código 3, una trama, tasa constante y 2 bytes de relleno.
        let mut packet = Vec::<u8>::new();
        packet.push(0b0000_0011u8);
        packet.push(0b0100_0001u8); // padding + 1 frame
        packet.push(2); // pad len
        packet.extend([0x42, 0x43, 0x44]); // frame data (3 bytes)
        packet.extend([0, 0]); // padding bytes

        let pp = parse_packet(&packet).unwrap();
        assert_eq!(pp.frame_count, 1);
        assert_eq!(pp.frames()[0], &[0x42, 0x43, 0x44]);
    }

    #[test]
    fn samples_per_frame_uses_config_low_bits() {
        // La configuración 19 (0b10011), solo CELT, es de 20 ms: 960 muestras a 48 kHz.
        // This validates we use config low bits (toc>>3)&0x03, not toc bits 5..6.
        let toc = 19u8 << 3;
        assert_eq!(samples_per_frame_48k(toc), 960);
    }

    #[test]
    #[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
    fn parse_official_vectors_smoke() {
        let dir = vectors_dir();
        let entries = fs::read_dir(&dir).unwrap_or_else(|e| {
            panic!("failed to read vectors dir {}: {e}", dir.to_string_lossy())
        });

        let mut bit_files = Vec::<PathBuf>::new();
        for ent in entries {
            let ent = ent.unwrap();
            let p = ent.path();
            if p.extension().and_then(|s| s.to_str()) == Some("bit") {
                bit_files.push(p);
            }
        }
        bit_files.sort();
        assert!(!bit_files.is_empty(), "no .bit files in {}", dir.display());

        for bit in bit_files {
            let records = read_opus_demo_records(&bit).unwrap_or_else(|e| {
                panic!("failed to read {}: {e}", bit.display());
            });
            for p in records.into_iter().filter_map(|r| r.packet) {
                parse_packet(&p).unwrap_or_else(|e| {
                    panic!(
                        "parse failed for {} packet len {}: {e}",
                        bit.display(),
                        p.len()
                    );
                });
            }
        }
    }
}

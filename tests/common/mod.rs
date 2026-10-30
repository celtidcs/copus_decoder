//! Ayudantes compartidos por varias pruebas de integración (`mod common;`).
//!
//! Cada prueba se compila como un crate aparte y usa solo una parte de este módulo, así que el
//! resto le parece código muerto. Por eso se permite `dead_code` aquí, y solo aquí.
#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Datos de un vector que usan las pruebas y las mediciones de rendimiento.
#[derive(Debug, Clone)]
pub struct VectorSpec {
    /// Ruta relativa del flujo `.bit`.
    pub bit: String,
    /// Frecuencia de salida con la que se decodifica.
    pub sample_rate: u32,
    /// Canales de salida con los que se decodifica.
    pub channels: usize,
}

/// Carpeta de los vectores, relativa a la raíz del crate. Es la que crean
/// `documentacion/infraestructura/preparar-entorno.ps1` y `.sh`. La misma constante está en
/// `src/test_vectors.rs`, para las pruebas unitarias; las dos deben coincidir.
const DEFAULT_VECTORS_DIR: &str = "material_secundario/vectores/opus_newvectors";
/// Variable de entorno que, si está definida, sustituye a la carpeta por defecto.
const VECTORS_DIR_ENV: &str = "OPUS_TESTVECTORS_DIR";
/// Bytes de cada campo de la cabecera de un registro de `opus_demo`: longitud y `final_range`,
/// cada uno un `u32` big-endian.
const FIELD_BYTES: usize = 4;

/// Un registro de un flujo `.bit` de `opus_demo`.
pub struct OpusDemoRecord {
    /// Carga del paquete; `None` si el registro marca un paquete perdido (longitud 0).
    pub packet: Option<Vec<u8>>,
    /// Estado final del decodificador de rango que obtuvo el decodificador de referencia.
    pub expected_final_range: u32,
}

/// Carpeta de los vectores: `OPUS_TESTVECTORS_DIR` si está definida y, si no, la carpeta por
/// defecto dentro del crate.
pub fn vectors_dir() -> PathBuf {
    std::env::var_os(VECTORS_DIR_ENV).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_VECTORS_DIR),
        PathBuf::from,
    )
}

/// Carpeta por defecto de las referencias de libopus 1.6.1, relativa a la raíz del crate.
const DEFAULT_REFERENCE_DIR: &str = "material_secundario/vectores/referencia-libopus-1.6.1";
/// Variable de entorno que, si está definida, sustituye a la carpeta de referencias por defecto.
const REFERENCE_DIR_ENV: &str = "OPUS_REFERENCIA_LIBOPUS_DIR";

/// Carpeta de las referencias: `OPUS_REFERENCIA_LIBOPUS_DIR` si está definida y, si no, la
/// carpeta por defecto dentro del crate.
pub fn reference_dir() -> PathBuf {
    std::env::var_os(REFERENCE_DIR_ENV).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_REFERENCE_DIR),
        PathBuf::from,
    )
}

/// Lee del manifiesto de los vectores de RFC la descripción de `vector_name` (por ejemplo,
/// `testvector07`).
pub fn load_vector_spec(vector_name: &str) -> io::Result<VectorSpec> {
    let manifest = fs::read_to_string(vectors_dir().join("vectors.txt"))?;
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut fields = line.split_whitespace();
        let name = fields.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "missing vector name in manifest",
            )
        })?;
        if name != vector_name {
            continue;
        }

        let bit = fields
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing .bit path"))?
            .to_string();
        let _dec = fields
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing .dec path"))?;
        let sample_rate = fields
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing sample rate"))?
            .parse::<u32>()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        let channels = fields
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing channel count"))?
            .parse::<usize>()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

        return Ok(VectorSpec {
            bit,
            sample_rate,
            channels,
        });
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("vector not found in manifest: {vector_name}"),
    ))
}

/// Paquetes del flujo `.bit` del vector oficial `vector_name` (por ejemplo, `testvector07`),
/// sin los registros de pérdida.
pub fn load_packets(vector_name: &str) -> io::Result<Vec<Vec<u8>>> {
    let spec = load_vector_spec(vector_name)?;
    let packets = read_opus_demo_stream(&vectors_dir().join(spec.bit))?;
    Ok(packets.into_iter().flatten().collect())
}

/// Lee un flujo `.bit` de `opus_demo`: registros de longitud (`u32` BE), `final_range` (`u32` BE)
/// y la carga.
///
/// Devuelve `UnexpectedEof` si el archivo acaba a mitad de un registro.
pub fn read_opus_demo_records(path: &Path) -> io::Result<Vec<OpusDemoRecord>> {
    let bytes = fs::read(path)?;
    let mut records = Vec::new();
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        let packet_len = take_u32_be(&mut rest, "truncated packet_len")? as usize;
        let expected_final_range = take_u32_be(&mut rest, "truncated final_range")?;
        if rest.len() < packet_len {
            return Err(truncated("truncated packet"));
        }
        let (payload, tail) = rest.split_at(packet_len);
        rest = tail;
        records.push(OpusDemoRecord {
            packet: (packet_len > 0).then(|| payload.to_vec()),
            expected_final_range,
        });
    }
    Ok(records)
}

/// Lee solo las cargas de un flujo `.bit`; `None` marca un paquete perdido.
pub fn read_opus_demo_stream(path: &Path) -> io::Result<Vec<Option<Vec<u8>>>> {
    Ok(read_opus_demo_records(path)?
        .into_iter()
        .map(|r| r.packet)
        .collect())
}

fn take_u32_be(rest: &mut &[u8], what: &'static str) -> io::Result<u32> {
    let (field, tail) = rest
        .split_first_chunk::<FIELD_BYTES>()
        .ok_or_else(|| truncated(what))?;
    *rest = tail;
    Ok(u32::from_be_bytes(*field))
}

fn truncated(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, what)
}

/// Construye un paquete de multiflujo con los `sub_packets` en orden de flujo, como
/// `opus_multistream_encode` de libopus: todos menos el último en formato autodelimitado
/// ([`to_self_delimited`]) y el último en el normal.
pub fn build_multistream_packet(sub_packets: &[&[u8]]) -> Vec<u8> {
    let mut packet = Vec::new();
    for (stream_idx, sub_packet) in sub_packets.iter().enumerate() {
        if stream_idx + 1 == sub_packets.len() {
            packet.extend_from_slice(sub_packet);
        } else {
            packet.extend(to_self_delimited(sub_packet));
        }
    }
    packet
}

/// Pasa un paquete normal al formato autodelimitado (RFC 6716, apéndice B), como lo lee
/// `opus_packet_parse_impl` de libopus 1.6.1 con `self_delimited` (`src/opus.c`, 222–357): la
/// longitud de la última trama (en el código 1 y en el 3 de tasa constante, la de cada trama) se
/// inserta tras el TOC, el byte de cuenta, el relleno y las demás longitudes; el relleno sigue al
/// final. Es la transcripción con la que se construye el oráculo de las pruebas, independiente
/// del analizador del crate.
///
/// No entra en pánico con un paquete mal formado: la prueba de no-pánico construye con él
/// multiflujos a partir de paquetes al azar. Entonces las cuentas saturan, la longitud se limita
/// a 1275 y el resultado es otro paquete mal formado, igual de determinista.
pub fn to_self_delimited(packet: &[u8]) -> Vec<u8> {
    let Some(&toc) = packet.first() else {
        return Vec::new();
    };
    let len = packet.len() - 1;
    let (header_end, last_size) = match toc & 0x03 {
        0 => (1, len),
        1 => (1, len / 2),
        2 => {
            let (first, used) = read_frame_size(&packet[1..]);
            (1 + used, len.saturating_sub(used + first))
        }
        _ => code3_self_delimited_parts(packet),
    };
    let header_end = header_end.min(packet.len());
    let mut out = packet[..header_end].to_vec();
    out.extend(encode_frame_size(last_size.min(MAX_FRAME_BYTES)));
    out.extend_from_slice(&packet[header_end..]);
    out
}

/// Bytes más largos que puede declarar una longitud de trama (RFC 6716 §3.2.1).
const MAX_FRAME_BYTES: usize = 1275;

/// Código 3 de [`to_self_delimited`]: fin de la cabecera (byte de cuenta, relleno y longitudes
/// explícitas) y longitud de la última trama (de cada trama, si la tasa es constante).
fn code3_self_delimited_parts(packet: &[u8]) -> (usize, usize) {
    let count_byte = packet.get(1).copied().unwrap_or(0);
    let count = usize::from(count_byte & 0x3f).max(1);
    let mut end = 2;
    let mut padding = 0usize;
    if count_byte & 0x40 != 0 {
        loop {
            let p = usize::from(packet.get(end).copied().unwrap_or(0));
            end += 1;
            padding += if p == 255 { 254 } else { p };
            if p != 255 {
                break;
            }
        }
    }
    let rest = packet.len().saturating_sub(end).saturating_sub(padding);
    if count_byte & 0x80 == 0 {
        return (end, rest / count);
    }
    let mut others = 0usize;
    for _ in 1..count {
        let (size, used) = read_frame_size(packet.get(end..).unwrap_or_default());
        end += used;
        others += size;
    }
    let rest = packet.len().saturating_sub(end).saturating_sub(padding);
    (end, rest.saturating_sub(others))
}

/// Lee una longitud de trama de uno o dos bytes (RFC 6716 §3.2.1): `(longitud, bytes)`. Los bytes
/// que falten cuentan como 0.
fn read_frame_size(data: &[u8]) -> (usize, usize) {
    let first = usize::from(data.first().copied().unwrap_or(0));
    if first < 252 {
        (first, 1)
    } else {
        (
            4 * usize::from(data.get(1).copied().unwrap_or(0)) + first,
            2,
        )
    }
}

/// Codifica una longitud de trama (como mucho 1275) en uno o dos bytes (RFC 6716 §3.2.1).
pub fn encode_frame_size(len: usize) -> Vec<u8> {
    if len < 252 {
        vec![u8::try_from(len).expect("una longitud corta cabe en un byte")]
    } else {
        let remainder = len - 252;
        vec![
            u8::try_from(252 + (remainder & 0x03)).expect("primer byte de la longitud"),
            u8::try_from(remainder >> 2).expect("una longitud de trama no pasa de 1275"),
        ]
    }
}

/// Paquetes híbridos de una trama, uno por configuración (12 a 15), cuya redundancia anunciada no
/// cabe en la trama. Se hallaron con una búsqueda aleatoria de 400 000 paquetes, en `release` y con
/// la comprobación de cordura desactivada a propósito: con ella quitada, cada uno hace entrar en
/// pánico al decodificador. El bit de redundancia de un paquete híbrido tiene probabilidad 1/4096
/// (RFC 6716 §4.5.1), así que el corpus aleatorio casi nunca llega a ese camino. Los usan `no_panico` y `fuzzing`.
pub const PAQUETES_REDUNDANCIA_QUE_NO_CABE: [&[u8]; 4] = [
    &[
        96, 67, 50, 20, 112, 124, 91, 35, 54, 240, 226, 28, 116, 243, 158, 9, 177, 185, 103, 252,
        104, 196, 127, 236, 111, 216, 89, 45, 129, 245, 133, 122, 66, 61, 247, 114, 49, 216, 1,
    ],
    &[
        104, 58, 181, 155, 204, 61, 41, 109, 160, 74, 100, 136, 2, 174, 125, 34, 246, 59, 247, 132,
        19, 225, 162, 21, 230, 174, 4, 6, 178, 250, 55, 7, 123, 28, 129, 233, 58, 29, 143, 189,
        111, 73, 29, 230, 139, 249, 203, 215, 2, 212, 134, 209, 157, 121, 139, 226, 234, 101, 26,
        182, 42, 31, 175, 158, 245, 60, 96, 114, 89, 57, 34, 51, 125, 245, 167, 188, 253, 46, 56,
        138, 46,
    ],
    &[
        112, 101, 100, 215, 39, 63, 140, 221, 88, 178, 225, 152, 175, 115, 191, 162, 39, 109, 223,
        189, 39, 61, 38, 162, 138, 194, 80, 0, 92, 9, 195, 27, 127, 58, 139, 77, 105, 231, 235,
        194, 17, 158, 149, 16, 208, 79, 236, 14, 39, 88, 88, 58, 216, 110, 224, 26, 96, 196, 60,
        45, 195, 18, 17, 109, 51, 136, 184, 112, 171, 234, 192, 226, 198, 235, 185, 105, 50,
    ],
    &[
        120, 140, 112, 32, 248, 255, 137, 53, 238, 24, 250, 238, 55, 238, 127, 82, 185, 59, 204,
        175, 120, 169, 100, 144, 67, 70, 237, 125, 189, 102, 229, 9, 69, 223, 41, 203, 202, 94,
        127, 49, 108, 229, 89, 123, 70, 133, 41, 118, 122, 56, 243, 213, 27, 81, 40, 40, 106, 98,
        169, 162, 28, 103, 46, 197, 81,
    ],
];

/// Generador xorshift64*: suficiente para variar la entrada y sin dependencias externas. Lo
/// usan `no_panico` y `fuzzing`; con la misma semilla da siempre la misma secuencia.
pub struct Generador(pub u64);

impl Generador {
    pub fn siguiente(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Entero uniforme en `0..n` (`n > 0`).
    pub fn hasta(&mut self, n: usize) -> usize {
        (self.siguiente() % n as u64) as usize
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.siguiente() as u8).collect()
    }
}

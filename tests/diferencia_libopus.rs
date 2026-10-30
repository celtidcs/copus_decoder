//! Barrera de proximidad a libopus: decodifica los 12 vectores de RFC 8251 a 48 kHz en estéreo y
//! compara la salida muestra a muestra con su referencia (`.dec`).
//!
//! La conformidad (`conformance_rfc.rs`) usa una medida de calidad que admite un margen, así que
//! no ve que la salida se aleje de libopus mientras siga dentro de él. Esta prueba fija, para cada
//! vector, la diferencia máxima y la suma de diferencias absolutas medidas con el decodificador
//! actual: cualquier cambio que aleje la salida de la referencia la pone roja. Si una corrección
//! la acerca, los topes se bajan en ese mismo cambio.

mod common;

use std::fs;

use common::{read_opus_demo_stream, vectors_dir};
use copus_decoder::OpusDecoder;

/// Frecuencia de salida de las referencias `.dec`.
const REFERENCE_RATE_HZ: u32 = 48_000;
/// Canales de salida de las referencias `.dec`.
const REFERENCE_CHANNELS: usize = 2;

/// Topes por vector: nombre, diferencia máxima y suma de diferencias absolutas, en unidades de
/// `i16`. Son las medidas del decodificador actual. La meta es 0; lo que queda (1, y 4 en el 08)
/// está en `documentacion/defectos-conocidos.md`.
const TOPES: [(&str, i32, u64); 12] = [
    ("testvector01", 1, 346),
    ("testvector02", 0, 0),
    ("testvector03", 0, 0),
    ("testvector04", 0, 0),
    ("testvector05", 1, 493),
    ("testvector06", 1, 519),
    ("testvector07", 1, 191),
    ("testvector08", 4, 5014),
    ("testvector09", 1, 317),
    ("testvector10", 1, 849),
    ("testvector11", 1, 553),
    ("testvector12", 1, 616),
];

/// Diferencia máxima y suma de diferencias absolutas entre dos señales, en sus muestras comunes.
fn diferencias(salida: &[i16], referencia: &[i16]) -> (i32, u64) {
    salida
        .iter()
        .zip(referencia)
        .map(|(&a, &b)| (i32::from(a) - i32::from(b)).abs())
        .fold((0, 0), |(maxima, suma), d| (maxima.max(d), suma + d as u64))
}

/// Decodifica el vector `nombre` a la frecuencia y canales de la referencia.
fn decodificar(nombre: &str) -> Vec<i16> {
    let paquetes = read_opus_demo_stream(&vectors_dir().join(format!("{nombre}.bit")))
        .unwrap_or_else(|e| panic!("{nombre}: no se puede leer el .bit: {e}"));
    let mut dec = OpusDecoder::new(REFERENCE_RATE_HZ, REFERENCE_CHANNELS)
        .unwrap_or_else(|e| panic!("{nombre}: no se puede crear el decodificador: {e}"));
    let canales = REFERENCE_CHANNELS;
    let mut trama = vec![0i16; dec.max_frame_size_per_channel() * canales];
    let mut salida = Vec::new();
    for paquete in paquetes {
        let paquete = paquete.unwrap_or_default();
        let n = dec
            .decode(&paquete, &mut trama, false)
            .unwrap_or_else(|e| panic!("{nombre}: error al decodificar: {e}"));
        salida.extend_from_slice(&trama[..n * canales]);
    }
    salida
}

/// Muestras `i16` little-endian de la referencia `.dec` del vector `nombre`.
fn referencia(nombre: &str) -> Vec<i16> {
    fs::read(vectors_dir().join(format!("{nombre}.dec")))
        .unwrap_or_else(|e| panic!("{nombre}: no se puede leer el .dec: {e}"))
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

#[test]
#[ignore = "necesita los vectores de RFC 8251 (OPUS_TESTVECTORS_DIR o la carpeta por defecto)"]
fn la_salida_no_se_aleja_de_libopus() {
    let mut fallos = Vec::new();
    for (nombre, max_tope, suma_tope) in TOPES {
        let (salida, referencia) = (decodificar(nombre), referencia(nombre));
        let (maxima, suma) = diferencias(&salida, &referencia);
        eprintln!("{nombre}: máxima {maxima} (tope {max_tope}), suma {suma} (tope {suma_tope})");
        if salida.len() != referencia.len() {
            fallos.push(format!(
                "{nombre}: {} muestras frente a {} de la referencia",
                salida.len(),
                referencia.len()
            ));
        }
        if maxima > max_tope || suma > suma_tope {
            fallos.push(format!(
                "{nombre}: máxima {maxima} > {max_tope} o suma {suma} > {suma_tope}"
            ));
        }
    }
    assert!(
        fallos.is_empty(),
        "la salida se aleja de libopus:\n{}",
        fallos.join("\n")
    );
}

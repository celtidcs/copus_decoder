//! Barrera del paso de un paquete SILK estéreo a uno mono con salida estéreo, como `silk_Decode`
//! de libopus 1.6.1 (`silk/dec_API.c`, `stereo_to_mono`): si la frecuencia interna no cambia, la
//! primera trama interna del paquete mono saca el canal derecho por el remuestreador del derecho,
//! que conserva el estado del tramo estéreo; las demás tramas internas, y todo el paquete si la
//! frecuencia cambia, copian el izquierdo.

use copus_decoder::OpusDecoder;

/// Paquete SILK estéreo de banda ancha y 20 ms (configuración 9), codificado con libopus 1.6.1:
/// el primer flujo del paquete 651 del caso multiflujo `varios` de la referencia de libopus.
const ESTEREO_WB_20MS: [u8; 34] = [
    0x4c, 0xa8, 0x00, 0xd6, 0xa0, 0xe5, 0x84, 0xd1, 0xa4, 0x7a, 0xaf, 0xa9, 0x35, 0x4a, 0x5a, 0x3a,
    0xf1, 0xd7, 0x18, 0xc5, 0x15, 0x31, 0xaa, 0xc5, 0x60, 0x47, 0xe5, 0xb4, 0x3a, 0x4e, 0x45, 0xbb,
    0x67, 0x70,
];

/// Paquete SILK mono de banda ancha y 40 ms (configuración 10, dos tramas internas de 20 ms), el
/// que sigue al anterior en `varios` (paquete 652).
const MONO_WB_40MS: [u8; 42] = [
    0x50, 0xd3, 0xae, 0x6d, 0x11, 0xc8, 0xf3, 0xca, 0xf2, 0xd9, 0x09, 0x09, 0xf5, 0x56, 0x12, 0xe0,
    0x14, 0xa5, 0x6b, 0x7c, 0xf8, 0x7a, 0xb4, 0xe5, 0x01, 0x75, 0xcb, 0x9d, 0xdb, 0x47, 0xe7, 0xc8,
    0x17, 0x30, 0x57, 0x4c, 0x69, 0x30, 0xef, 0x38, 0x0f, 0xe0,
];

/// Paquete SILK mono de banda estrecha y 20 ms (configuración 1), de otra frecuencia interna que
/// los anteriores (paquete 1030 de `varios`).
const MONO_NB_20MS: [u8; 20] = [
    0x08, 0xb2, 0xbd, 0x3b, 0x9a, 0xbd, 0x53, 0x9d, 0x9c, 0xe6, 0xc4, 0xa7, 0xa7, 0xe3, 0x01, 0x50,
    0xf1, 0x3e, 0x36, 0xc0,
];

/// Frecuencias de salida admitidas; con cada una el remuestreo de SILK es distinto.
const FRECUENCIAS: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// Duración de una trama interna de SILK de 20 ms, en milisegundos.
const TRAMA_INTERNA_MS: usize = 20;

/// Muestras por canal de una trama interna de 20 ms a `frecuencia` Hz.
fn trama_interna(frecuencia: u32) -> usize {
    frecuencia as usize * TRAMA_INTERNA_MS / 1000
}

/// Decodifica a `frecuencia` Hz en estéreo `ESTEREO_WB_20MS` y después `mono`, y devuelve los
/// canales izquierdo y derecho de la salida de `mono`.
fn canales_tras_estereo(mono: &[u8], frecuencia: u32) -> (Vec<i16>, Vec<i16>) {
    let mut dec = OpusDecoder::new(frecuencia, 2).unwrap();
    let mut pcm = vec![0i16; OpusDecoder::MAX_FRAME_SIZE_48K * 2];
    dec.decode(&ESTEREO_WB_20MS, &mut pcm, false).unwrap();
    let n = dec.decode(mono, &mut pcm, false).unwrap();
    let canal = |c: usize| pcm[..n * 2].iter().skip(c).step_by(2).copied().collect();
    (canal(0), canal(1))
}

/// A la misma frecuencia interna, el derecho de la primera trama interna sale del remuestreador
/// del derecho, que a 48 kHz conserva estado del tramo estéreo: no es una copia del izquierdo.
#[test]
fn a_la_misma_frecuencia_la_primera_trama_usa_el_remuestreador_del_derecho() {
    let (izquierdo, derecho) = canales_tras_estereo(&MONO_WB_40MS, 48_000);
    let primera = ..trama_interna(48_000);
    assert!(
        izquierdo[primera] != derecho[primera],
        "el derecho de la primera trama interna es una copia del izquierdo"
    );
}

/// A la misma frecuencia interna, el derecho de la segunda trama interna copia el izquierdo a
/// todas las frecuencias de salida.
#[test]
fn a_la_misma_frecuencia_la_segunda_trama_copia_el_izquierdo() {
    for frecuencia in FRECUENCIAS {
        let (izquierdo, derecho) = canales_tras_estereo(&MONO_WB_40MS, frecuencia);
        let trama = trama_interna(frecuencia);
        assert_eq!(izquierdo.len(), 2 * trama, "a {frecuencia} Hz");
        assert!(
            izquierdo[trama..] == derecho[trama..],
            "a {frecuencia} Hz, el derecho de la segunda trama interna no copia el izquierdo"
        );
    }
}

/// Si la frecuencia interna cambia, el derecho copia el izquierdo en todo el paquete, a todas
/// las frecuencias de salida.
#[test]
fn con_otra_frecuencia_el_derecho_copia_el_izquierdo() {
    for frecuencia in FRECUENCIAS {
        let (izquierdo, derecho) = canales_tras_estereo(&MONO_NB_20MS, frecuencia);
        assert_eq!(
            izquierdo.len(),
            trama_interna(frecuencia),
            "a {frecuencia} Hz"
        );
        assert!(
            izquierdo == derecho,
            "a {frecuencia} Hz, el derecho no copia el izquierdo tras cambiar la frecuencia interna"
        );
    }
}

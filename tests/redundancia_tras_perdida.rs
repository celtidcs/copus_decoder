//! Barrera de la redundancia CELT→SILK tras una pérdida, como `opus_decode_frame` de libopus
//! 1.6.1 (`src/opus_decoder.c`): la trama redundante de un paquete solo SILK que sigue a uno solo
//! CELT se decodifica siempre, pero solo se mezcla en la salida si la trama anterior no era solo
//! SILK. Si la anterior se perdió y se ocultó con SILK, no se mezcla.
//!
//! Los datos son los 51 primeros registros de `opus_demo` del primer flujo del multiflujo `varios`
//! de la referencia de libopus, con ráfagas de 3 pérdidas (`varios_f0_r3`): 47 paquetes, la
//! pérdida de los registros 47 a 49 (uno solo SILK y dos solo CELT) y el paquete 50, solo SILK
//! de banda ancha en estéreo, con redundancia CELT→SILK. La salida esperada es la de libopus 1.6.1
//! para el paquete 50 a 48 kHz en estéreo (`opus_demo -d 48000 2`), en `i16` little-endian.

use copus_decoder::OpusDecoder;

/// Registros de `opus_demo` (longitud y estado de rango en big-endian, y el paquete); los vacíos
/// son pérdidas.
const SECUENCIA: &[u8] = include_bytes!("datos/redundancia_tras_perdida.bit");
/// Salida de libopus 1.6.1 del último paquete de `SECUENCIA`.
const SALIDA_LIBOPUS: &[u8] = include_bytes!("datos/redundancia_tras_perdida.dec");

const FRECUENCIA: u32 = 48_000;
const CANALES: usize = 2;
/// Bytes de la cabecera de cada registro de `opus_demo`: longitud y estado de rango.
const CABECERA_REGISTRO: usize = 8;

/// Paquetes de `SECUENCIA`, con `None` en las pérdidas.
fn registros() -> Vec<Option<&'static [u8]>> {
    let mut resto = SECUENCIA;
    let mut paquetes = Vec::new();
    while resto.len() >= CABECERA_REGISTRO {
        let largo = u32::from_be_bytes(resto[..4].try_into().unwrap()) as usize;
        let paquete = &resto[CABECERA_REGISTRO..CABECERA_REGISTRO + largo];
        paquetes.push((largo > 0).then_some(paquete));
        resto = &resto[CABECERA_REGISTRO + largo..];
    }
    paquetes
}

/// Decodifica `SECUENCIA` como `opus_demo -d`: las pérdidas se ocultan al llegar el paquete
/// siguiente, la última con FEC si ese paquete trae LBRR. Devuelve la salida del último paquete.
fn salida_del_ultimo_paquete() -> Vec<i16> {
    let mut dec = OpusDecoder::new(FRECUENCIA, CANALES).unwrap();
    let mut pcm = vec![0i16; OpusDecoder::MAX_FRAME_SIZE_48K * CANALES];
    let mut perdidas = 0usize;
    let mut ultima = Vec::new();
    for paquete in registros() {
        let Some(paquete) = paquete else {
            perdidas += 1;
            continue;
        };
        let con_lbrr = copus_decoder::packet_has_lbrr(paquete).unwrap();
        for k in 0..perdidas {
            if k + 1 == perdidas && con_lbrr {
                dec.decode(paquete, &mut pcm, true).unwrap();
            } else {
                dec.decode(&[], &mut pcm, false).unwrap();
            }
        }
        perdidas = 0;
        let n = dec.decode(paquete, &mut pcm, false).unwrap();
        ultima = pcm[..n * CANALES].to_vec();
    }
    ultima
}

#[test]
fn tras_ocultar_en_silk_la_redundancia_celt_a_silk_no_se_mezcla() {
    let esperada: Vec<i16> = SALIDA_LIBOPUS
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let salida = salida_del_ultimo_paquete();
    assert_eq!(salida.len(), esperada.len(), "duración del último paquete");
    let distintas: Vec<(usize, i16, i16)> = salida
        .iter()
        .zip(&esperada)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (&a, &b))| (i / CANALES, a, b))
        .collect();
    assert!(
        distintas.is_empty(),
        "{} muestras distintas de libopus; las primeras (muestra por canal, nuestra, libopus): {:?}",
        distintas.len(),
        &distintas[..distintas.len().min(8)]
    );
}

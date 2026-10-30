//! Objetivo de fuzzing de `OpusDecoder`: secuencias de paquetes, pérdidas y FEC, en las 10
//! configuraciones de salida, con `decode` y `decode_float` a la vez. Además de la ausencia de
//! pánicos, comprueba que los dos dan lo mismo y que nada pasa del tamaño de la salida.
#![no_main]

mod comun;

use comun::{DIVISOR_SALIDA_CORTA, Entrada, FRECUENCIAS, comprobar_iguales};
use copus_decoder::OpusDecoder;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|datos: &[u8]| {
    let mut entrada = Entrada::new(datos);
    let Some(eleccion) = entrada.byte() else {
        return;
    };
    let eleccion = usize::from(eleccion);
    let frecuencia = FRECUENCIAS[eleccion % FRECUENCIAS.len()];
    let canales = 1 + eleccion / FRECUENCIAS.len() % 2;
    let mut entero = OpusDecoder::new(frecuencia, canales).expect("configuración válida");
    let mut flotante = OpusDecoder::new(frecuencia, canales).expect("configuración válida");
    let completa = OpusDecoder::MAX_FRAME_SIZE_48K * canales;
    let mut pcm = vec![0i16; completa];
    let mut pcm_f = vec![0f32; completa];
    while let Some(llamada) = entrada.llamada() {
        let largo = if llamada.salida_corta {
            completa / DIVISOR_SALIDA_CORTA
        } else {
            completa
        };
        let a = entero.decode(llamada.paquete, &mut pcm[..largo], llamada.fec);
        let b = flotante.decode_float(llamada.paquete, &mut pcm_f[..largo], llamada.fec);
        comprobar_iguales(a, b, &pcm[..largo], &pcm_f[..largo], canales);
    }
});

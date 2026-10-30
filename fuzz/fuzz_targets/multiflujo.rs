//! Objetivo de fuzzing de `OpusMultistreamDecoder`: secuencias de paquetes multiflujo, pérdidas y
//! FEC en cuatro topologías (una con un canal mudo) y todas las frecuencias, con `decode` y
//! `decode_float` a la vez. Comprueba lo mismo que el objetivo de un solo flujo.
#![no_main]

mod comun;

use comun::{DIVISOR_SALIDA_CORTA, Entrada, FRECUENCIAS, comprobar_iguales};
use copus_decoder::{OpusDecoder, OpusMultistreamDecoder};
use libfuzzer_sys::fuzz_target;

/// Topologías: (canales, flujos, flujos acoplados, mapa de canales). El canal 255 queda mudo.
const TOPOLOGIAS: [(usize, usize, usize, &[u8]); 4] = [
    (2, 2, 0, &[0, 1]),
    (3, 2, 1, &[0, 1, 2]),
    (6, 4, 2, &[0, 4, 1, 2, 3, 5]),
    (4, 2, 1, &[0, 255, 1, 2]),
];

fuzz_target!(|datos: &[u8]| {
    let mut entrada = Entrada::new(datos);
    let Some(eleccion) = entrada.byte() else {
        return;
    };
    let eleccion = usize::from(eleccion);
    let frecuencia = FRECUENCIAS[eleccion % FRECUENCIAS.len()];
    let (canales, flujos, acoplados, mapa) =
        TOPOLOGIAS[eleccion / FRECUENCIAS.len() % TOPOLOGIAS.len()];
    let nuevo = || {
        OpusMultistreamDecoder::new(frecuencia, canales, flujos, acoplados, mapa)
            .expect("configuración válida")
    };
    let (mut entero, mut flotante) = (nuevo(), nuevo());
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

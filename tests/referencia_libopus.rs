//! Barreras contra salidas de referencia de libopus 1.6.1 con pérdidas y FEC en banda.
//!
//! Las referencias se generan compilando libopus 1.6.1 sin las aproximaciones rápidas de coma
//! flotante y decodificando con su API `opus_decode` (`decodificar.c`, con la política de pérdidas
//! de `opus_demo -d`) flujos con pérdidas, y con su API multiflujo flujos de tres canales, con
//! `documentacion/infraestructura/referencia-libopus/generar.sh`; las pruebas las buscan en
//! `OPUS_REFERENCIA_LIBOPUS_DIR` o, si no está definida, en
//! `material_secundario/vectores/referencia-libopus-1.6.1`. Están marcadas `#[ignore]` porque
//! dependen de esos datos externos.

mod common;

use std::fs;
use std::path::Path;

use common::{read_opus_demo_stream, reference_dir};
use copus_decoder::{OpusDecoder, OpusMultistreamDecoder};

/// Paquetes con LBRR que cuenta `opus_packet_has_lbrr` de libopus 1.6.1 en cada flujo de la
/// referencia (herramienta `lbrr.c`, con la que se generó). Los flujos de los vectores de
/// RFC 8251 no traen ninguno.
const LIBOPUS_LBRR_COUNTS: [(&str, usize); 4] = [
    ("fec_32000", 917),
    ("fec_32000_p9", 822),
    ("fec_32000_r3", 870),
    ("estereo_fec", 792),
];

/// Frecuencia y canales de salida de las referencias (`decodificar 48000 2`).
const REFERENCE_RATE_HZ: u32 = 48_000;
const REFERENCE_CHANNELS: usize = 2;

/// Diferencia máxima y suma de diferencias absolutas, en unidades de `i16`, de una clase de
/// tramas frente a la referencia.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Diferencia {
    max: i32,
    suma: u64,
}

impl Diferencia {
    fn sumar(&mut self, salida: &[i16], referencia: &[i16]) {
        for (&a, &b) in salida.iter().zip(referencia) {
            let d = (i32::from(a) - i32::from(b)).abs();
            self.max = self.max.max(d);
            self.suma += d as u64;
        }
    }

    fn cabe_en(self, tope: Self) -> bool {
        self.max <= tope.max && self.suma <= tope.suma
    }
}

/// Diferencias por clase de trama: las buenas, las perdidas que se ocultan y las perdidas que se
/// recuperan con FEC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PorClase {
    buenas: Diferencia,
    ocultadas: Diferencia,
    fec: Diferencia,
}

impl PorClase {
    /// Si cada clase de `self` cabe en la misma clase de `tope`.
    fn cabe_en(self, tope: Self) -> bool {
        self.buenas.cabe_en(tope.buenas)
            && self.ocultadas.cabe_en(tope.ocultadas)
            && self.fec.cabe_en(tope.fec)
    }
}

/// Topes de los flujos con FEC en banda: las diferencias medidas con el decodificador actual.
/// Las tramas buenas, las recuperadas y las ocultadas son idénticas a las de libopus.
const TOPES_FEC: [(&str, PorClase); 3] = [
    ("fec_32000", TOPE_FEC_EXACTO),
    ("fec_32000_p9", TOPE_FEC_EXACTO),
    ("fec_32000_r3", TOPE_FEC_EXACTO),
];
/// El tope común de los flujos con FEC: ninguna diferencia.
const TOPE_FEC_EXACTO: PorClase = PorClase {
    buenas: Diferencia { max: 0, suma: 0 },
    ocultadas: Diferencia { max: 0, suma: 0 },
    fec: Diferencia { max: 0, suma: 0 },
};

/// Decodifica el flujo `name` de la referencia como `decodificar.c` y `opus_demo -d`
/// (`src/opus_demo.c`, v1.6.1):
/// las pérdidas se emiten al llegar el siguiente paquete bueno; la última de cada racha se
/// recupera con FEC si ese paquete trae LBRR, y las del final del flujo no se emiten. Compara
/// cada trama con la salida de referencia y devuelve las diferencias por clase.
fn diferencias_como_opus_demo(name: &str) -> PorClase {
    let dir = reference_dir();
    diferencias_en_formato(
        &dir.join("flujos").join(format!("{name}.bit")),
        &dir.join("salidas").join(format!("{name}.dec")),
        REFERENCE_RATE_HZ,
        REFERENCE_CHANNELS,
    )
}

/// Como [`diferencias_como_opus_demo`], con el flujo `flujo` decodificado a `rate_hz` y
/// `channels` canales (`decodificar <rate_hz> <channels>`) y comparado con la salida `salida`.
fn diferencias_en_formato(flujo: &Path, salida: &Path, rate_hz: u32, channels: usize) -> PorClase {
    let name = flujo.display();
    let packets =
        read_opus_demo_stream(flujo).unwrap_or_else(|e| panic!("no se puede leer {name}: {e}"));
    let referencia: Vec<i16> = fs::read(salida)
        .unwrap_or_else(|e| panic!("no se puede leer {}: {e}", salida.display()))
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    let mut dec = OpusDecoder::new(rate_hz, channels).unwrap();
    let mut out = vec![0i16; dec.max_frame_size_per_channel() * channels];
    let mut clases = PorClase::default();
    let (mut pos, mut pendientes) = (0usize, 0usize);
    let comparar = |destino: &mut Diferencia, out: &[i16], n: usize, pos: &mut usize| {
        let fin = (*pos + n).min(referencia.len());
        destino.sumar(&out[..fin - *pos], &referencia[*pos..fin]);
        *pos += n;
    };
    for packet in packets {
        let Some(packet) = packet else {
            pendientes += 1;
            continue;
        };
        let con_lbrr = copus_decoder::packet_has_lbrr(&packet).unwrap();
        for fr in 0..pendientes {
            let recuperable = fr + 1 == pendientes && con_lbrr;
            let n = if recuperable {
                dec.decode(&packet, &mut out, true)
            } else {
                dec.decode(&[], &mut out, false)
            }
            .unwrap()
                * channels;
            let destino = if recuperable {
                &mut clases.fec
            } else {
                &mut clases.ocultadas
            };
            comparar(destino, &out, n, &mut pos);
        }
        pendientes = 0;
        let n = dec.decode(&packet, &mut out, false).unwrap() * channels;
        comparar(&mut clases.buenas, &out, n, &mut pos);
    }
    assert_eq!(
        pos,
        referencia.len(),
        "{name}: longitud distinta de la referencia"
    );
    clases
}

/// Paquetes con LBRR del flujo `name` de la referencia, según [`copus_decoder::packet_has_lbrr`].
fn lbrr_count(name: &str) -> usize {
    let path = reference_dir().join("flujos").join(format!("{name}.bit"));
    let packets = read_opus_demo_stream(&path)
        .unwrap_or_else(|e| panic!("no se puede leer {}: {e}", path.display()));
    packets
        .iter()
        .flatten()
        .filter(|packet| {
            copus_decoder::packet_has_lbrr(packet)
                .unwrap_or_else(|e| panic!("{name}: paquete no válido: {e}"))
        })
        .count()
}

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn packet_has_lbrr_cuenta_lo_mismo_que_libopus() {
    for (name, expected) in LIBOPUS_LBRR_COUNTS {
        assert_eq!(lbrr_count(name), expected, "{name}");
    }
    for k in 1..=12 {
        for pattern in ["p9", "r3"] {
            let name = format!("testvector{k:02}_{pattern}");
            assert_eq!(lbrr_count(&name), 0, "{name}");
        }
    }
}

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn el_fec_no_se_aleja_de_libopus() {
    let mut fallos = Vec::new();
    for (name, tope) in TOPES_FEC {
        let medida = diferencias_como_opus_demo(name);
        eprintln!("{name}: {medida:?}");
        let clases = [
            ("buenas", medida.buenas, tope.buenas),
            ("ocultadas", medida.ocultadas, tope.ocultadas),
            ("fec", medida.fec, tope.fec),
        ];
        for (clase, valor, maximo) in clases {
            if !valor.cabe_en(maximo) {
                fallos.push(format!("{name}, {clase}: {valor:?} > {maximo:?}"));
            }
        }
    }
    assert!(
        fallos.is_empty(),
        "el FEC se aleja de libopus:\n{}",
        fallos.join("\n")
    );
}

/// Flujos con pérdidas de los vectores solo SILK de RFC 8251 (02, 03 y 04: banda estrecha, media
/// y ancha; tramas de 10 a 60 ms; mono y estéreo), que sin pérdidas ya salen idénticos a libopus,
/// y el flujo SILK con DTX (`dtx`: paquetes de un byte, que se ocultan con ruido de confort), solo
/// y con pérdidas.
const FLUJOS_SOLO_SILK: [&str; 9] = [
    "testvector02_p9",
    "testvector02_r3",
    "testvector03_p9",
    "testvector03_r3",
    "testvector04_p9",
    "testvector04_r3",
    "dtx",
    "dtx_p9",
    "dtx_r3",
];

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn la_ocultacion_silk_es_la_de_libopus() {
    let fallos = flujos_distintos_de_libopus(&FLUJOS_SOLO_SILK);
    assert!(
        fallos.is_empty(),
        "la ocultación de SILK no es la de libopus:\n{}",
        fallos.join("\n")
    );
}

/// Vectores de RFC 8251 con paquetes híbridos y sin paquetes solo CELT, con pérdidas: el 05 y el
/// 06, todo híbrido, y el 12, híbrido y solo SILK.
const FLUJOS_HIBRIDOS_CON_PERDIDAS: [&str; 6] = [
    "testvector05_p9",
    "testvector05_r3",
    "testvector06_p9",
    "testvector06_r3",
    "testvector12_p9",
    "testvector12_r3",
];
/// Diferencia máxima con libopus de esos vectores sin pérdidas (redondeo de coma flotante).
const RESIDUO_HIBRIDO_SIN_PERDIDAS: i32 = 1;

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn la_ocultacion_hibrida_es_la_de_libopus() {
    // `opus_decode_frame` de libopus oculta una trama híbrida con SILK y con el ruido de CELT
    // desde la banda 17, en trozos de como mucho 20 ms; la trama buena que sigue hereda el estado
    // de los dos. Ni las ocultadas ni las buenas deben alejarse más que sin pérdidas.
    let fallos: Vec<String> = FLUJOS_HIBRIDOS_CON_PERDIDAS
        .iter()
        .filter_map(|name| {
            let medida = diferencias_como_opus_demo(name);
            let peor = medida.buenas.max.max(medida.ocultadas.max);
            (peor > RESIDUO_HIBRIDO_SIN_PERDIDAS).then(|| format!("{name}: {medida:?}"))
        })
        .collect();
    assert!(
        fallos.is_empty(),
        "la ocultación híbrida no es la de libopus:
{}",
        fallos.join(
            "
"
        )
    );
}

/// Topes de los vectores de RFC 8251 con paquetes solo CELT, con pérdidas, a 48 kHz en estéreo:
/// las diferencias medidas con el decodificador actual. La ocultación periódica sigue la de libopus
/// 1.6.1 en x86-64 con AVX2 y queda en la diferencia que estos vectores tienen sin pérdidas (1 o 2;
/// 4 en el 08), salvo en el 08_p9: su trama buena 432 difiere hasta 6279 porque libopus aplica a
/// la salida `i16` un recorte suave (`opus_pcm_soft_clip`) que este decodificador no tiene.
const TOPES_CELT_CON_PERDIDAS: [(&str, PorClase); 12] = [
    ("testvector01_p9", por_clase((1, 342), (1, 2797))),
    ("testvector01_r3", por_clase((1, 45), (1, 733))),
    ("testvector07_p9", por_clase((1, 1092), (2, 1173))),
    ("testvector07_r3", por_clase((1, 125), (1, 48))),
    ("testvector08_p9", por_clase((6279, 527_963), (1, 1201))),
    ("testvector08_r3", por_clase((4, 3942), (1, 807))),
    ("testvector09_p9", por_clase((1, 354), (1, 992))),
    ("testvector09_r3", por_clase((1, 257), (1, 64))),
    ("testvector10_p9", por_clase((1, 9172), (1, 7308))),
    ("testvector10_r3", por_clase((1, 2076), (1, 3334))),
    ("testvector11_p9", por_clase((1, 3), (1, 36))),
    ("testvector11_r3", por_clase((1, 3), (1, 91))),
];

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn la_ocultacion_celt_es_la_de_libopus() {
    // `celt_decode_lost` de libopus repite el periodo de tono de la señal anterior en las
    // pérdidas de paquetes solo CELT (`FRAME_PLC_PERIODIC`). Ni las ocultadas ni las buenas que
    // las siguen deben alejarse más de lo medido.
    let fallos: Vec<String> = TOPES_CELT_CON_PERDIDAS
        .iter()
        .filter_map(|&(name, tope)| {
            let medida = diferencias_como_opus_demo(name);
            (!medida.cabe_en(tope)).then(|| format!("{name}: {medida:?} supera {tope:?}"))
        })
        .collect();
    assert!(
        fallos.is_empty(),
        "la ocultación de CELT no es la de libopus:\n{}",
        fallos.join("\n")
    );
}

/// Flujo `empalmes` (paquetes de los vectores 02, 03 y 04 de RFC 8251 de 7 en 7: cambios de
/// frecuencia interna de SILK y pasos de mono a estéreo) solo y con pérdidas.
const FLUJOS_EMPALMES: [&str; 3] = ["empalmes", "empalmes_p9", "empalmes_r3"];

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn los_empalmes_son_los_de_libopus() {
    let fallos = flujos_distintos_de_libopus(&FLUJOS_EMPALMES);
    assert!(
        fallos.is_empty(),
        "los cambios de frecuencia y de canales de SILK no son los de libopus:\n{}",
        fallos.join("\n")
    );
}

/// Secuencias de ocultación de `generar.sh` (10 paquetes, una racha de pérdidas y 10 paquetes) con
/// sus topes a 48 kHz en estéreo y en mono: las diferencias medidas con el decodificador actual.
/// La de SILK (vector 02, 5 pérdidas) es idéntica a libopus; la de CELT (vector 07, 3 pérdidas)
/// tiene la diferencia de 1 que el vector deja también sin pérdidas.
const SECUENCIAS_PLC: [(&str, PorClase, PorClase); 2] = [
    (
        "plc_celt",
        por_clase((1, 2), (0, 0)),
        por_clase((1, 1), (0, 0)),
    ),
    (
        "plc_silk",
        por_clase((0, 0), (0, 0)),
        por_clase((0, 0), (0, 0)),
    ),
];

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn la_secuencia_de_ocultacion_es_la_de_libopus() {
    // La energía de la ocultación no tiene por qué bajar en cada pérdida: en la secuencia SILK,
    // la de libopus sube en la cuarta. Lo que se exige es la salida de libopus.
    let mut fallos = Vec::new();
    for &(name, tope_estereo, tope_mono) in &SECUENCIAS_PLC {
        let medidas = [
            ("48000/2", diferencias_como_opus_demo(name), tope_estereo),
            (
                "48000/1",
                diferencias_en_frecuencia(name, MONO_RATE_HZ, MONO_CHANNELS),
                tope_mono,
            ),
        ];
        for (formato, medida, tope) in medidas {
            if !medida.cabe_en(tope) {
                fallos.push(format!("{name} {formato}: {medida:?} supera {tope:?}"));
            }
        }
        for channels in [MONO_CHANNELS, REFERENCE_CHANNELS] {
            comprobar_enteros_y_flotantes(name, channels);
        }
    }
    assert!(
        fallos.is_empty(),
        "la secuencia de ocultación no es la de libopus:\n{}",
        fallos.join("\n")
    );
}

/// Decodifica la secuencia `name` de la referencia a 48 kHz con `channels` canales con `decode` y
/// con `decode_float` a la vez, perdiendo los registros vacíos, y comprueba que cada llamada da
/// la misma duración con las dos y que todas las muestras en coma flotante son finitas.
fn comprobar_enteros_y_flotantes(name: &str, channels: usize) {
    let flujo = reference_dir().join("flujos").join(format!("{name}.bit"));
    let packets =
        read_opus_demo_stream(&flujo).unwrap_or_else(|e| panic!("no se puede leer {name}: {e}"));
    let mut dec_i16 = OpusDecoder::new(REFERENCE_RATE_HZ, channels).unwrap();
    let mut dec_f32 = OpusDecoder::new(REFERENCE_RATE_HZ, channels).unwrap();
    let mut pcm_i16 = vec![0i16; dec_i16.max_frame_size_per_channel() * channels];
    let mut pcm_f32 = vec![0.0f32; pcm_i16.len()];
    for (i, packet) in packets.iter().enumerate() {
        let packet = packet.as_deref().unwrap_or_default();
        let n_i16 = dec_i16.decode(packet, &mut pcm_i16, false).unwrap();
        let n_f32 = dec_f32.decode_float(packet, &mut pcm_f32, false).unwrap();
        assert_eq!(
            n_i16, n_f32,
            "{name}/{channels}, registro {i}: duraciones distintas"
        );
        assert!(n_f32 > 0, "{name}/{channels}, registro {i}: sin muestras");
        assert!(
            pcm_f32[..n_f32 * channels].iter().all(|s| s.is_finite()),
            "{name}/{channels}, registro {i}: muestras no finitas"
        );
    }
}

/// Flujos solo SILK con salidas de libopus a otras frecuencias y canales (`frecuencias/`): los
/// vectores 02, 03 y 04 de RFC 8251 y los flujos `dtx` y `empalmes`, solos y con pérdidas.
const FLUJOS_SILK_EN_FORMATOS: [&str; 15] = [
    "testvector02",
    "testvector02_p9",
    "testvector02_r3",
    "testvector03",
    "testvector03_p9",
    "testvector03_r3",
    "testvector04",
    "testvector04_p9",
    "testvector04_r3",
    "dtx",
    "dtx_p9",
    "dtx_r3",
    "empalmes",
    "empalmes_p9",
    "empalmes_r3",
];

/// Salida mono a 48 kHz (`decodificar 48000 1`).
const MONO_RATE_HZ: u32 = 48_000;
const MONO_CHANNELS: usize = 1;

/// Topes de los flujos del vector 12 (sin pérdidas, `_p9` y `_r3`) en un formato de salida.
type TopesTv12 = [(&'static str, PorClase); 3];

/// Topes del vector 12 (híbrido, mono) a 48 kHz en mono: las diferencias medidas con el
/// decodificador actual. Como a 48 kHz en estéreo, como mucho 1, también en las tramas ocultadas.
const TOPES_MONO_TV12: TopesTv12 = [
    ("testvector12", por_clase((1, 306), (0, 0))),
    ("testvector12_p9", por_clase((1, 247), (0, 0))),
    ("testvector12_r3", por_clase((1, 279), (0, 0))),
];

/// Diferencias de las tramas buenas y de las ocultadas, como `(máximo, suma)`, sin tramas
/// recuperadas con FEC.
const fn por_clase(buenas: (i32, u64), ocultadas: (i32, u64)) -> PorClase {
    PorClase {
        buenas: Diferencia {
            max: buenas.0,
            suma: buenas.1,
        },
        ocultadas: Diferencia {
            max: ocultadas.0,
            suma: ocultadas.1,
        },
        fec: Diferencia { max: 0, suma: 0 },
    }
}

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn la_salida_mono_es_la_de_libopus() {
    let fallos = fallos_en_formato(MONO_RATE_HZ, MONO_CHANNELS, &TOPES_MONO_TV12);
    assert!(
        fallos.is_empty(),
        "la salida mono no es la de libopus:\n{}",
        fallos.join("\n")
    );
}

/// Formatos de salida distintos de 48 kHz de las referencias de `frecuencias/`
/// (`decodificar <frecuencia> <canales>`), con los topes del vector 12 en cada uno: las
/// diferencias medidas con el decodificador actual. A 24 kHz, como a 48 kHz, como mucho 1. A 16, 12 y
/// 8 kHz, como mucho 1 en las tramas buenas y nada en las ocultadas: allí el Nyquist de la salida
/// cae en el comienzo de la parte CELT de los paquetes híbridos (8 kHz) o por debajo, así que esa
/// parte se anula antes de diezmar (antes de recortarla, se replegaba y difería hasta 453). A 16 y
/// 8 kHz en estéreo, igual que en mono.
const FORMATOS_NO_48K: [(u32, usize, TopesTv12); 6] = [
    (
        24_000,
        2,
        [
            ("testvector12", por_clase((1, 274), (0, 0))),
            ("testvector12_p9", por_clase((1, 238), (1, 1))),
            ("testvector12_r3", por_clase((1, 256), (0, 0))),
        ],
    ),
    (
        16_000,
        1,
        [
            ("testvector12", por_clase((1, 53), (0, 0))),
            ("testvector12_p9", por_clase((1, 51), (0, 0))),
            ("testvector12_r3", por_clase((1, 53), (0, 0))),
        ],
    ),
    (
        16_000,
        2,
        [
            ("testvector12", por_clase((1, 106), (0, 0))),
            ("testvector12_p9", por_clase((1, 102), (0, 0))),
            ("testvector12_r3", por_clase((1, 106), (0, 0))),
        ],
    ),
    (
        12_000,
        2,
        [
            ("testvector12", por_clase((1, 76), (0, 0))),
            ("testvector12_p9", por_clase((1, 88), (0, 0))),
            ("testvector12_r3", por_clase((1, 80), (0, 0))),
        ],
    ),
    (
        8_000,
        1,
        [
            ("testvector12", por_clase((1, 29), (0, 0))),
            ("testvector12_p9", por_clase((1, 27), (0, 0))),
            ("testvector12_r3", por_clase((1, 27), (0, 0))),
        ],
    ),
    (
        8_000,
        2,
        [
            ("testvector12", por_clase((1, 58), (0, 0))),
            ("testvector12_p9", por_clase((1, 54), (0, 0))),
            ("testvector12_r3", por_clase((1, 54), (0, 0))),
        ],
    ),
];

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn las_frecuencias_son_las_de_libopus() {
    let fallos: Vec<String> = FORMATOS_NO_48K
        .iter()
        .flat_map(|(rate_hz, channels, topes)| {
            fallos_en_formato(*rate_hz, *channels, topes)
                .into_iter()
                .map(move |fallo| format!("{rate_hz}/{channels} {fallo}"))
        })
        .collect();
    assert!(
        fallos.is_empty(),
        "la salida fuera de 48 kHz no es la de libopus:
{}",
        fallos.join(
            "
"
        )
    );
}

/// Tope de un flujo decodificado a una frecuencia y unos canales: nombre del flujo, frecuencia,
/// canales y diferencias máximas por clase.
type TopeEnFormato = (&'static str, u32, usize, PorClase);

/// Topes de los vectores de RFC 8251 con paquetes solo CELT (01, 07, 10 y 11), solos y con
/// pérdidas, a las frecuencias y canales de salida distintos de 48 kHz en estéreo: las diferencias
/// medidas con el decodificador actual. Sin pérdidas, como mucho 1, como a 48 kHz en estéreo. Con
/// pérdidas, las de más de 5 son ocultaciones fuera de 48 kHz, donde la predicción lineal de la
/// ocultación amplifica diferencias mínimas en la memoria (defectos conocidos): el 07_p9, hasta
/// 156 en las ocultadas a 16 kHz en estéreo y 97 a 8 kHz; y el 01_r3 a 12 kHz en mono, (91, 296),
/// que era (1, 3) antes de calcular la rotación de `exp_rotation` como libopus: en sus pérdidas
/// 247 a 249 una diferencia de 1,3·10⁻⁷ en la memoria cambia los coeficientes del filtro un 3,6 %,
/// y no se sabe cuál de las dos memorias es la de libopus.
const TOPES_CELT_EN_FORMATOS: [TopeEnFormato; 108] = [
    ("testvector01", 12_000, 1, por_clase((1, 1), (0, 0))),
    ("testvector01", 12_000, 2, por_clase((0, 0), (0, 0))),
    ("testvector01", 16_000, 1, por_clase((1, 4), (0, 0))),
    ("testvector01", 16_000, 2, por_clase((1, 4), (0, 0))),
    ("testvector01", 24_000, 1, por_clase((0, 0), (0, 0))),
    ("testvector01", 24_000, 2, por_clase((1, 4), (0, 0))),
    ("testvector01", 48_000, 1, por_clase((1, 3), (0, 0))),
    ("testvector01", 8_000, 1, por_clase((0, 0), (0, 0))),
    ("testvector01", 8_000, 2, por_clase((1, 2), (0, 0))),
    ("testvector01_p9", 12_000, 1, por_clase((1, 64), (2, 332))),
    ("testvector01_p9", 12_000, 2, por_clase((3, 309), (4, 2225))),
    ("testvector01_p9", 16_000, 1, por_clase((1, 96), (2, 752))),
    ("testvector01_p9", 16_000, 2, por_clase((3, 197), (2, 1392))),
    ("testvector01_p9", 24_000, 1, por_clase((1, 94), (1, 599))),
    ("testvector01_p9", 24_000, 2, por_clase((1, 147), (1, 1070))),
    ("testvector01_p9", 48_000, 1, por_clase((1, 171), (1, 1043))),
    ("testvector01_p9", 8_000, 1, por_clase((1, 42), (2, 219))),
    ("testvector01_p9", 8_000, 2, por_clase((1, 44), (2, 354))),
    (
        "testvector01_r3",
        12_000,
        1,
        por_clase((91, 1706), (296, 22_976)),
    ),
    ("testvector01_r3", 12_000, 2, por_clase((1, 68), (3, 866))),
    ("testvector01_r3", 16_000, 1, por_clase((1, 47), (1, 375))),
    ("testvector01_r3", 16_000, 2, por_clase((2, 79), (2, 698))),
    ("testvector01_r3", 24_000, 1, por_clase((3, 113), (3, 828))),
    ("testvector01_r3", 24_000, 2, por_clase((1, 69), (2, 698))),
    ("testvector01_r3", 48_000, 1, por_clase((1, 19), (1, 225))),
    ("testvector01_r3", 8_000, 1, por_clase((1, 9), (1, 79))),
    ("testvector01_r3", 8_000, 2, por_clase((2, 20), (4, 205))),
    ("testvector07", 12_000, 1, por_clase((1, 4), (0, 0))),
    ("testvector07", 12_000, 2, por_clase((1, 8), (0, 0))),
    ("testvector07", 16_000, 1, por_clase((1, 14), (0, 0))),
    ("testvector07", 16_000, 2, por_clase((1, 28), (0, 0))),
    ("testvector07", 24_000, 1, por_clase((1, 25), (0, 0))),
    ("testvector07", 24_000, 2, por_clase((1, 50), (0, 0))),
    ("testvector07", 48_000, 1, por_clase((1, 72), (0, 0))),
    ("testvector07", 8_000, 1, por_clase((0, 0), (0, 0))),
    ("testvector07", 8_000, 2, por_clase((0, 0), (0, 0))),
    ("testvector07_p9", 12_000, 1, por_clase((9, 329), (27, 300))),
    ("testvector07_p9", 12_000, 2, por_clase((9, 625), (27, 615))),
    ("testvector07_p9", 16_000, 1, por_clase((1, 238), (1, 227))),
    (
        "testvector07_p9",
        16_000,
        2,
        por_clase((19, 1530), (156, 2581)),
    ),
    ("testvector07_p9", 24_000, 1, por_clase((1, 170), (2, 245))),
    ("testvector07_p9", 24_000, 2, por_clase((1, 532), (2, 614))),
    ("testvector07_p9", 48_000, 1, por_clase((1, 341), (2, 439))),
    ("testvector07_p9", 8_000, 1, por_clase((4, 106), (97, 533))),
    ("testvector07_p9", 8_000, 2, por_clase((4, 214), (97, 1048))),
    ("testvector07_r3", 12_000, 1, por_clase((1, 9), (2, 39))),
    ("testvector07_r3", 12_000, 2, por_clase((1, 16), (1, 14))),
    ("testvector07_r3", 16_000, 1, por_clase((1, 9), (1, 8))),
    ("testvector07_r3", 16_000, 2, por_clase((1, 21), (1, 15))),
    ("testvector07_r3", 24_000, 1, por_clase((1, 28), (1, 16))),
    ("testvector07_r3", 24_000, 2, por_clase((1, 57), (1, 32))),
    ("testvector07_r3", 48_000, 1, por_clase((1, 59), (1, 23))),
    ("testvector07_r3", 8_000, 1, por_clase((1, 2), (1, 1))),
    ("testvector07_r3", 8_000, 2, por_clase((1, 6), (1, 12))),
    ("testvector10", 12_000, 1, por_clase((1, 45), (0, 0))),
    ("testvector10", 12_000, 2, por_clase((1, 86), (0, 0))),
    ("testvector10", 16_000, 1, por_clase((1, 60), (0, 0))),
    ("testvector10", 16_000, 2, por_clase((1, 122), (0, 0))),
    ("testvector10", 24_000, 1, por_clase((1, 182), (0, 0))),
    ("testvector10", 24_000, 2, por_clase((1, 362), (0, 0))),
    ("testvector10", 48_000, 1, por_clase((1, 379), (0, 0))),
    ("testvector10", 8_000, 1, por_clase((1, 35), (0, 0))),
    ("testvector10", 8_000, 2, por_clase((1, 68), (0, 0))),
    (
        "testvector10_p9",
        12_000,
        1,
        por_clase((2, 1683), (2, 1317)),
    ),
    (
        "testvector10_p9",
        12_000,
        2,
        por_clase((2, 3624), (2, 3203)),
    ),
    (
        "testvector10_p9",
        16_000,
        1,
        por_clase((2, 2510), (2, 1908)),
    ),
    (
        "testvector10_p9",
        16_000,
        2,
        por_clase((1, 3707), (1, 2975)),
    ),
    (
        "testvector10_p9",
        24_000,
        1,
        por_clase((2, 2981), (1, 2208)),
    ),
    (
        "testvector10_p9",
        24_000,
        2,
        por_clase((2, 6275), (2, 5841)),
    ),
    (
        "testvector10_p9",
        48_000,
        1,
        por_clase((1, 5497), (1, 4757)),
    ),
    ("testvector10_p9", 8_000, 1, por_clase((1, 702), (1, 739))),
    ("testvector10_p9", 8_000, 2, por_clase((1, 1696), (1, 1547))),
    ("testvector10_r3", 12_000, 1, por_clase((1, 108), (1, 263))),
    ("testvector10_r3", 12_000, 2, por_clase((1, 180), (1, 475))),
    ("testvector10_r3", 16_000, 1, por_clase((1, 192), (1, 674))),
    ("testvector10_r3", 16_000, 2, por_clase((1, 370), (1, 1484))),
    ("testvector10_r3", 24_000, 1, por_clase((1, 295), (1, 1012))),
    ("testvector10_r3", 24_000, 2, por_clase((1, 593), (1, 2062))),
    (
        "testvector10_r3",
        48_000,
        1,
        por_clase((1, 1096), (1, 1985)),
    ),
    ("testvector10_r3", 8_000, 1, por_clase((1, 101), (1, 275))),
    ("testvector10_r3", 8_000, 2, por_clase((1, 187), (1, 538))),
    ("testvector11", 12_000, 1, por_clase((0, 0), (0, 0))),
    ("testvector11", 12_000, 2, por_clase((0, 0), (0, 0))),
    ("testvector11", 16_000, 1, por_clase((0, 0), (0, 0))),
    ("testvector11", 16_000, 2, por_clase((1, 2), (0, 0))),
    ("testvector11", 24_000, 1, por_clase((0, 0), (0, 0))),
    ("testvector11", 24_000, 2, por_clase((1, 1), (0, 0))),
    ("testvector11", 48_000, 1, por_clase((1, 1), (0, 0))),
    ("testvector11", 8_000, 1, por_clase((1, 1), (0, 0))),
    ("testvector11", 8_000, 2, por_clase((0, 0), (0, 0))),
    ("testvector11_p9", 12_000, 1, por_clase((1, 1), (1, 2))),
    ("testvector11_p9", 12_000, 2, por_clase((0, 0), (1, 9))),
    ("testvector11_p9", 16_000, 1, por_clase((1, 1), (1, 8))),
    ("testvector11_p9", 16_000, 2, por_clase((1, 2), (1, 12))),
    ("testvector11_p9", 24_000, 1, por_clase((1, 2), (1, 9))),
    ("testvector11_p9", 24_000, 2, por_clase((1, 1), (1, 20))),
    ("testvector11_p9", 48_000, 1, por_clase((1, 2), (1, 37))),
    ("testvector11_p9", 8_000, 1, por_clase((0, 0), (1, 123))),
    ("testvector11_p9", 8_000, 2, por_clase((1, 4), (1, 123))),
    ("testvector11_r3", 12_000, 1, por_clase((1, 8), (1, 402))),
    ("testvector11_r3", 12_000, 2, por_clase((1, 6), (2, 389))),
    ("testvector11_r3", 16_000, 1, por_clase((1, 8), (2, 601))),
    ("testvector11_r3", 16_000, 2, por_clase((1, 3), (1, 64))),
    ("testvector11_r3", 24_000, 1, por_clase((1, 23), (2, 1177))),
    ("testvector11_r3", 24_000, 2, por_clase((1, 2), (1, 287))),
    ("testvector11_r3", 48_000, 1, por_clase((1, 1), (1, 155))),
    ("testvector11_r3", 8_000, 1, por_clase((1, 2), (1, 47))),
    ("testvector11_r3", 8_000, 2, por_clase((1, 4), (1, 334))),
];

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn los_vectores_celt_a_otras_frecuencias_no_se_alejan_de_libopus() {
    let fallos = fallos_por_tope(&TOPES_CELT_EN_FORMATOS);
    assert!(
        fallos.is_empty(),
        "los vectores con CELT fuera de 48 kHz en estéreo se alejan de libopus:\n{}",
        fallos.join("\n")
    );
}

/// Flujo con pasos de estéreo a mono y FEC en banda (`estereo_fec.c`: la salida de referencia del
/// vector 02 codificada a 32 kb/s con FEC, alternando cada 7 paquetes estéreo y mono; 89 pasos de
/// estéreo a mono), solo y con pérdidas.
const FLUJOS_ESTEREO_FEC: [&str; 3] = ["estereo_fec", "estereo_fec_p9", "estereo_fec_r3"];
/// Sus formatos de salida: 48 kHz en estéreo y los de los flujos SILK en `generar.sh`.
const FORMATOS_ESTEREO_FEC: [(u32, usize); 8] = [
    (48_000, 2),
    (48_000, 1),
    (24_000, 2),
    (16_000, 2),
    (16_000, 1),
    (12_000, 2),
    (8_000, 2),
    (8_000, 1),
];

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn el_paso_de_estereo_a_mono_con_fec_es_el_de_libopus() {
    let fallos: Vec<String> = FLUJOS_ESTEREO_FEC
        .iter()
        .flat_map(|name| {
            FORMATOS_ESTEREO_FEC
                .iter()
                .map(move |&(rate_hz, channels)| (name, rate_hz, channels))
        })
        .filter_map(|(name, rate_hz, channels)| {
            let medida = diferencias_en(name, rate_hz, channels);
            (medida != PorClase::default())
                .then(|| format!("{name} {rate_hz}/{channels}: {medida:?}"))
        })
        .collect();
    assert!(
        fallos.is_empty(),
        "el paso de estéreo a mono con FEC no es el de libopus:\n{}",
        fallos.join("\n")
    );
}

/// Topes del primer flujo de `ms_varios` (`primer_flujo.c`: estéreo, con cambios de modo, de
/// canales, de banda y de duración), solo y con pérdidas, a todas las frecuencias y canales: las
/// diferencias medidas con el decodificador actual. Sin pérdidas, como mucho 1, y con ráfagas de
/// 3 pérdidas, 2. Con una pérdida de cada 9 quedan restos de hasta 24 en paquetes solo CELT que
/// siguen a una pérdida CELT, sin investigar (defectos conocidos); las tramas ocultadas difieren en
/// 1 como mucho. Antes de no mezclar la redundancia CELT→SILK tras una ocultación SILK, como
/// libopus, llegaban a 2316.
const TOPES_VARIOS_F0: [TopeEnFormato; 30] = [
    ("varios_f0", 12_000, 1, por_clase((1, 909), (0, 0))),
    ("varios_f0", 12_000, 2, por_clase((1, 1888), (0, 0))),
    ("varios_f0", 16_000, 1, por_clase((1, 1317), (0, 0))),
    ("varios_f0", 16_000, 2, por_clase((1, 2556), (0, 0))),
    ("varios_f0", 24_000, 1, por_clase((1, 1817), (0, 0))),
    ("varios_f0", 24_000, 2, por_clase((1, 3750), (0, 0))),
    ("varios_f0", 48_000, 1, por_clase((1, 3780), (0, 0))),
    ("varios_f0", 48_000, 2, por_clase((1, 7661), (0, 0))),
    ("varios_f0", 8_000, 1, por_clase((1, 615), (0, 0))),
    ("varios_f0", 8_000, 2, por_clase((1, 1218), (0, 0))),
    ("varios_f0_p9", 12_000, 1, por_clase((22, 1133), (0, 0))),
    ("varios_f0_p9", 12_000, 2, por_clase((22, 2455), (1, 1))),
    ("varios_f0_p9", 16_000, 1, por_clase((23, 1558), (0, 0))),
    ("varios_f0_p9", 16_000, 2, por_clase((23, 3261), (0, 0))),
    ("varios_f0_p9", 24_000, 1, por_clase((23, 2268), (0, 0))),
    ("varios_f0_p9", 24_000, 2, por_clase((23, 4967), (1, 22))),
    ("varios_f0_p9", 48_000, 1, por_clase((24, 4685), (0, 0))),
    ("varios_f0_p9", 48_000, 2, por_clase((24, 10_136), (1, 10))),
    ("varios_f0_p9", 8_000, 1, por_clase((22, 776), (0, 0))),
    ("varios_f0_p9", 8_000, 2, por_clase((22, 1633), (1, 7))),
    ("varios_f0_r3", 12_000, 1, por_clase((1, 885), (1, 1))),
    ("varios_f0_r3", 12_000, 2, por_clase((2, 1823), (1, 2))),
    ("varios_f0_r3", 16_000, 1, por_clase((1, 1241), (0, 0))),
    ("varios_f0_r3", 16_000, 2, por_clase((1, 2470), (0, 0))),
    ("varios_f0_r3", 24_000, 1, por_clase((1, 1755), (0, 0))),
    ("varios_f0_r3", 24_000, 2, por_clase((2, 3611), (1, 1))),
    ("varios_f0_r3", 48_000, 1, por_clase((1, 3654), (0, 0))),
    ("varios_f0_r3", 48_000, 2, por_clase((2, 7391), (1, 1))),
    ("varios_f0_r3", 8_000, 1, por_clase((1, 598), (0, 0))),
    ("varios_f0_r3", 8_000, 2, por_clase((2, 1185), (1, 2))),
];

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn el_primer_flujo_de_varios_no_se_aleja_de_libopus() {
    let fallos = fallos_por_tope(&TOPES_VARIOS_F0);
    assert!(
        fallos.is_empty(),
        "el primer flujo de varios se aleja de libopus:\n{}",
        fallos.join("\n")
    );
}

/// Los casos de `topes` cuya diferencia con libopus pasa de su tope, con sus diferencias.
fn fallos_por_tope(topes: &[TopeEnFormato]) -> Vec<String> {
    topes
        .iter()
        .filter_map(|&(name, rate_hz, channels, tope)| {
            let medida = diferencias_en(name, rate_hz, channels);
            (!medida.cabe_en(tope))
                .then(|| format!("{name} {rate_hz}/{channels}: {medida:?} supera {tope:?}"))
        })
        .collect()
}

/// Diferencias del flujo `name` decodificado a `rate_hz` y `channels` canales frente a la salida
/// de libopus: la de `salidas/` a 48 kHz en estéreo y la de `frecuencias/` en los demás formatos.
fn diferencias_en(name: &str, rate_hz: u32, channels: usize) -> PorClase {
    if (rate_hz, channels) == (REFERENCE_RATE_HZ, REFERENCE_CHANNELS) {
        diferencias_como_opus_demo(name)
    } else {
        diferencias_en_frecuencia(name, rate_hz, channels)
    }
}

/// Los flujos decodificados a `rate_hz` y `channels` canales que se apartan de libopus: los de
/// [`FLUJOS_SILK_EN_FORMATOS`] con cualquier diferencia y los de `topes_tv12` por encima de su
/// tope, con sus diferencias.
fn fallos_en_formato(
    rate_hz: u32,
    channels: usize,
    topes_tv12: &[(&str, PorClase)],
) -> Vec<String> {
    let mut fallos: Vec<String> = FLUJOS_SILK_EN_FORMATOS
        .iter()
        .filter_map(|name| {
            let medida = diferencias_en_frecuencia(name, rate_hz, channels);
            (medida != PorClase::default()).then(|| format!("{name}: {medida:?}"))
        })
        .collect();
    for &(name, tope) in topes_tv12 {
        let medida = diferencias_en_frecuencia(name, rate_hz, channels);
        if !medida.cabe_en(tope) {
            fallos.push(format!("{name}: {medida:?} supera {tope:?}"));
        }
    }
    fallos
}

/// Diferencias del flujo `name` decodificado a `rate_hz` y `channels` canales frente a la salida
/// de libopus de `frecuencias/`. Los flujos sin pérdidas de los vectores de RFC 8251 se leen de
/// la carpeta de los vectores; los demás, de la de la referencia.
fn diferencias_en_frecuencia(name: &str, rate_hz: u32, channels: usize) -> PorClase {
    let dir = reference_dir();
    let es_vector_rfc = name.starts_with("testvector") && !name.contains('_');
    let flujo = if es_vector_rfc {
        common::vectors_dir().join(format!("{name}.bit"))
    } else {
        dir.join("flujos").join(format!("{name}.bit"))
    };
    diferencias_en_formato(
        &flujo,
        &dir.join("frecuencias")
            .join(format!("{name}_{rate_hz}_{channels}.dec")),
        rate_hz,
        channels,
    )
}

/// Los flujos de `names` cuya salida no es idéntica a la de libopus en alguna clase de trama, con
/// sus diferencias.
fn flujos_distintos_de_libopus(names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter_map(|name| {
            let medida = diferencias_como_opus_demo(name);
            (medida != PorClase::default()).then(|| format!("{name}: {medida:?}"))
        })
        .collect()
}

/// Byte cualquiera con que se alarga a dos bytes un paquete DTX de solo TOC.
const BYTE_DE_RELLENO_DTX: u8 = 0xA5;

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn una_trama_de_un_byte_se_oculta_como_una_vacia() {
    // `opus_decode_frame` de libopus oculta igual una trama de 0 bytes que una de 1 («Payloads
    // of 1 (2 including ToC) or 0 trigger the PLC/DTX»). El flujo `dtx` solo trae paquetes DTX
    // de solo TOC (trama de 0 bytes); alargados a 2 bytes, la salida debe ser la misma.
    let flujo = reference_dir().join("flujos").join("dtx.bit");
    let packets = read_opus_demo_stream(&flujo)
        .unwrap_or_else(|e| panic!("no se puede leer {}: {e}", flujo.display()));
    let mut tal_cual = OpusDecoder::new(REFERENCE_RATE_HZ, REFERENCE_CHANNELS).unwrap();
    let mut alargado = OpusDecoder::new(REFERENCE_RATE_HZ, REFERENCE_CHANNELS).unwrap();
    let max = tal_cual.max_frame_size_per_channel() * REFERENCE_CHANNELS;
    let (mut a, mut b) = (vec![0i16; max], vec![0i16; max]);
    let mut alargados = 0;
    for (i, packet) in packets.iter().flatten().enumerate() {
        let mut otro = packet.clone();
        if packet.len() == 1 {
            otro.push(BYTE_DE_RELLENO_DTX);
            alargados += 1;
        }
        let na = tal_cual.decode(packet, &mut a, false).unwrap();
        let nb = alargado.decode(&otro, &mut b, false).unwrap();
        assert_eq!((na, &a[..na * 2]), (nb, &b[..nb * 2]), "paquete {i}");
    }
    assert!(alargados > 0, "el flujo dtx no trae paquetes de solo TOC");
}

/// Diferencias de un caso multiflujo: tramas de paquetes buenos y tramas perdidas.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PorPerdida {
    buenas: Diferencia,
    perdidas: Diferencia,
}

/// Atajo para escribir los topes de un caso multiflujo.
const fn por_perdida(buenas: (i32, u64), perdidas: (i32, u64)) -> PorPerdida {
    PorPerdida {
        buenas: Diferencia {
            max: buenas.0,
            suma: buenas.1,
        },
        perdidas: Diferencia {
            max: perdidas.0,
            suma: perdidas.1,
        },
    }
}

/// Casos de la referencia multiflujo (`multiflujo/ms_<caso>.bit` y `.dec`, generados con
/// `msref.c` contra libopus 1.6.1: 3 canales, 2 flujos, 1 acoplado, mapa {0, 1, 2}) y sus topes,
/// las diferencias medidas con el decodificador actual. En los cuatro primeros, la diferencia es
/// la de un solo flujo (máximo 1; `voip24`, exacto). En `fec` queda una diferencia de hasta 207;
/// sin comprobar, se atribuye a los paquetes híbridos del primer flujo, cuya capa CELT no se
/// oculta como en libopus. En `varios` (tramas de 2,5 a 60 ms y tasas de 12 a 192 kb/s alternadas),
/// el primer flujo, con sus pasos de estéreo a mono, difiere en 1 como mucho; en el segundo, las
/// transiciones de modo sin redundancia, fundidas como libopus con la ocultación del modo
/// anterior, quedan en 14 como mucho, y el máximo, 17, es de tramas híbridas sin cambio de modo
/// (defectos conocidos).
const TOPES_MULTIFLUJO: [(&str, PorPerdida); 6] = [
    ("audio128", por_perdida((1, 29), (0, 0))),
    ("voip24", por_perdida((0, 0), (0, 0))),
    ("audio64_60ms", por_perdida((1, 16), (0, 0))),
    ("cbr48", por_perdida((1, 6), (0, 0))),
    ("fec", por_perdida((159, 3_192_463), (207, 649_342))),
    ("varios", por_perdida((17, 13_783), (0, 0))),
];
/// Topología de la referencia multiflujo.
const MULTIFLUJO_CANALES: usize = 3;
const MULTIFLUJO_FLUJOS: usize = 2;
const MULTIFLUJO_ACOPLADOS: usize = 1;
const MULTIFLUJO_MAPA: [u8; MULTIFLUJO_CANALES] = [0, 1, 2];

/// Decodifica el caso multiflujo `caso` como `msref.c`: una pérdida seguida de un paquete bueno
/// se pide con `fec` sobre ese paquete y, si no, se oculta; y compara con la salida de libopus.
/// Devuelve un error si el decodificador rechaza un paquete o la longitud no coincide.
fn diferencias_multiflujo(caso: &str) -> Result<PorPerdida, String> {
    let dir = reference_dir().join("multiflujo");
    let flujo = dir.join(format!("ms_{caso}.bit"));
    let packets = read_opus_demo_stream(&flujo)
        .unwrap_or_else(|e| panic!("no se puede leer {}: {e}", flujo.display()));
    let referencia: Vec<i16> = fs::read(dir.join(format!("ms_{caso}.dec")))
        .unwrap_or_else(|e| panic!("{caso}: no se puede leer la salida de referencia: {e}"))
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    let mut dec = OpusMultistreamDecoder::new(
        REFERENCE_RATE_HZ,
        MULTIFLUJO_CANALES,
        MULTIFLUJO_FLUJOS,
        MULTIFLUJO_ACOPLADOS,
        &MULTIFLUJO_MAPA,
    )
    .unwrap();
    let mut out = vec![0i16; OpusDecoder::MAX_FRAME_SIZE_48K * MULTIFLUJO_CANALES];
    let mut medida = PorPerdida::default();
    let mut pos = 0usize;
    for (n, packet) in packets.iter().enumerate() {
        let siguiente = packets.get(n + 1).and_then(Option::as_deref);
        let (resultado, destino) = match (packet, siguiente) {
            (Some(packet), _) => (dec.decode(packet, &mut out, false), &mut medida.buenas),
            (None, Some(siguiente)) => {
                (dec.decode(siguiente, &mut out, true), &mut medida.perdidas)
            }
            (None, None) => (dec.decode(&[], &mut out, false), &mut medida.perdidas),
        };
        let muestras =
            resultado.map_err(|e| format!("{caso}, paquete {n}: {e}"))? * MULTIFLUJO_CANALES;
        let fin = (pos + muestras).min(referencia.len());
        destino.sumar(&out[..fin - pos], &referencia[pos..fin]);
        pos += muestras;
    }
    if pos != referencia.len() {
        return Err(format!(
            "{caso}: {pos} muestras, la referencia tiene {}",
            referencia.len()
        ));
    }
    Ok(medida)
}

#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn el_multiflujo_no_se_aleja_de_libopus() {
    let mut fallos = Vec::new();
    for (caso, tope) in TOPES_MULTIFLUJO {
        let medida = match diferencias_multiflujo(caso) {
            Ok(medida) => medida,
            Err(e) => {
                fallos.push(e);
                continue;
            }
        };
        eprintln!("{caso}: {medida:?}");
        let clases = [
            ("buenas", medida.buenas, tope.buenas),
            ("perdidas", medida.perdidas, tope.perdidas),
        ];
        for (clase, valor, maximo) in clases {
            if !valor.cabe_en(maximo) {
                fallos.push(format!("{caso}, {clase}: {valor:?} > {maximo:?}"));
            }
        }
    }
    assert!(
        fallos.is_empty(),
        "el multiflujo se aleja de libopus:\n{}",
        fallos.join("\n")
    );
}

/// Flujo con FEC en banda cuyos paquetes forman los dos flujos acoplados de la barrera del FEC en
/// multiflujo.
const FLUJO_FEC_MULTIFLUJO: &str = "fec_32000_p9";

/// Barrera del FEC en multiflujo con oráculo por flujo: dos flujos acoplados con los mismos
/// paquetes de un flujo con LBRR y pérdidas, decodificados como `opus_demo` (la última pérdida de
/// cada racha, con `fec` sobre el paquete siguiente si trae LBRR), dan muestra a muestra lo mismo
/// que un `OpusDecoder` estéreo con los mismos subpaquetes y el mismo `fec`, como
/// `opus_multistream_decode_native` de libopus, que pasa `decode_fec` a cada flujo.
#[test]
#[ignore = "necesita las referencias de libopus 1.6.1 (OPUS_REFERENCIA_LIBOPUS_DIR o la carpeta por defecto)"]
fn el_fec_del_multiflujo_es_el_de_cada_flujo() {
    let flujo = reference_dir()
        .join("flujos")
        .join(format!("{FLUJO_FEC_MULTIFLUJO}.bit"));
    let packets = read_opus_demo_stream(&flujo)
        .unwrap_or_else(|e| panic!("no se puede leer {}: {e}", flujo.display()));
    let max = OpusDecoder::MAX_FRAME_SIZE_48K;
    let mut multi = OpusMultistreamDecoder::new(REFERENCE_RATE_HZ, 4, 2, 2, &[0, 1, 2, 3]).unwrap();
    let mut estereo = OpusDecoder::new(REFERENCE_RATE_HZ, REFERENCE_CHANNELS).unwrap();
    let mut comparar = |paquete: &[u8], fec: bool, n: usize| {
        let mut est = vec![0i16; max * REFERENCE_CHANNELS];
        let muestras = estereo.decode(paquete, &mut est, fec).unwrap();
        let multi_packet = if paquete.is_empty() {
            Vec::new()
        } else {
            common::build_multistream_packet(&[paquete, paquete])
        };
        let mut pcm = vec![0i16; max * 4];
        let resultado = multi.decode(&multi_packet, &mut pcm, fec);
        assert!(
            matches!(resultado, Ok(m) if m == muestras),
            "paquete {n}, fec={fec}: {resultado:?} (se esperaba Ok({muestras}))"
        );
        for (i, trama) in pcm[..muestras * 4].chunks_exact(4).enumerate() {
            let esperado = [est[2 * i], est[2 * i + 1]];
            assert!(
                trama[..2] == esperado && trama[2..] == esperado,
                "paquete {n}, fec={fec}, muestra {i}"
            );
        }
    };
    let mut pendientes = 0usize;
    let mut recuperadas = 0usize;
    for (n, packet) in packets.iter().enumerate() {
        let Some(packet) = packet else {
            pendientes += 1;
            continue;
        };
        let con_lbrr = copus_decoder::packet_has_lbrr(packet).unwrap();
        for fr in 0..pendientes {
            let recuperable = fr + 1 == pendientes && con_lbrr;
            if recuperable {
                recuperadas += 1;
                comparar(packet, true, n);
            } else {
                comparar(&[], false, n);
            }
        }
        pendientes = 0;
        comparar(packet, false, n);
    }
    assert!(
        recuperadas > 0,
        "el flujo no tiene pérdidas recuperables con FEC"
    );
}

//! Barrera de no-pánico: el decodificador debe devolver `Ok` o `Err` ante cualquier entrada,
//! nunca entrar en pánico.
//!
//! Los paquetes se generan con una semilla fija, así que cada ejecución recorre exactamente los
//! mismos casos. Cada decodificación se aísla con `catch_unwind` y un gancho de pánico anota su
//! lugar, para informar de todos los pánicos distintos y no solo del primero. Ejecutadas en el
//! perfil de depuración, las pruebas también detectan los desbordamientos aritméticos.

mod common;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Once;

use copus_decoder::{OpusDecoder, OpusMultistreamDecoder};

/// Semilla del generador; fija para que los casos sean reproducibles.
const SEMILLA: u64 = 0x5EED_C0DE_0BAD_F00D;
/// Frecuencias de salida que admite el decodificador (RFC 6716 §2).
const FRECUENCIAS: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
/// Muestras por canal de la duración máxima de un paquete: 120 ms a 48 kHz (RFC 6716 §3.2.5).
const MAX_MUESTRAS_POR_CANAL: usize = 5_760;
/// Longitud máxima de los paquetes aleatorios; algo mayor que los 1275 bytes de una trama
/// (RFC 6716 §3.2.1) para recorrer también los paquetes demasiado largos.
const MAX_BYTES_ALEATORIOS: usize = 1_500;
/// Tamaño máximo de una trama (RFC 6716 §3.2.1); es también el límite de los subpaquetes
/// autodelimitados del multiflujo.
const MAX_BYTES_TRAMA: usize = 1_275;
/// Longitud máxima de la carga tras un TOC válido.
const MAX_CARGA_TRAS_TOC: usize = 400;
/// Paquetes por configuración de salida en la prueba sintética.
const PAQUETES_POR_CONFIGURACION: usize = 1_500;

/// Generador xorshift64*: suficiente para variar la entrada y sin dependencias externas.
struct Generador(u64);

impl Generador {
    fn siguiente(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Entero uniforme en `0..n` (`n > 0`).
    fn hasta(&mut self, n: usize) -> usize {
        (self.siguiente() % n as u64) as usize
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.siguiente() as u8).collect()
    }
}

thread_local! {
    /// Verdadero mientras este hilo ejecuta un caso aislado.
    static AISLADO: Cell<bool> = const { Cell::new(false) };
    /// Lugar y mensaje del último pánico aislado de este hilo, anotados por el gancho.
    static ULTIMO_PANICO: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

/// Instala, una sola vez, un gancho que anota los pánicos de los casos aislados en lugar de
/// imprimirlos; cualquier otro pánico (el informe final, un fallo del arnés) se imprime como
/// siempre.
fn instalar_gancho() {
    static UNA_VEZ: Once = Once::new();
    UNA_VEZ.call_once(|| {
        let por_defecto = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if !AISLADO.with(Cell::get) {
                por_defecto(info);
                return;
            }
            let lugar = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_default();
            let mensaje = info
                .payload()
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| info.payload().downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            ULTIMO_PANICO.with(|p| *p.borrow_mut() = Some((lugar, mensaje)));
        }));
    });
}

/// Pánicos agrupados por lugar: número de casos, mensaje y el primer caso que lo provocó.
#[derive(Default)]
struct Registro(BTreeMap<String, (usize, String, String)>);

impl Registro {
    /// Ejecuta `f` aislada; devuelve `false` si entró en pánico y lo anota.
    fn ejecutar(&mut self, caso: impl FnOnce() -> String, f: impl FnOnce()) -> bool {
        AISLADO.with(|a| a.set(true));
        let resultado = panic::catch_unwind(AssertUnwindSafe(f));
        AISLADO.with(|a| a.set(false));
        if resultado.is_ok() {
            return true;
        }
        let (lugar, mensaje) = ULTIMO_PANICO
            .with(|p| p.borrow_mut().take())
            .unwrap_or_default();
        let entrada = self.0.entry(lugar).or_insert_with(|| (0, mensaje, caso()));
        entrada.0 += 1;
        false
    }

    fn comprobar(self, prueba: &str) {
        if self.0.is_empty() {
            return;
        }
        let mut informe = format!("{prueba}: {} lugares de pánico distintos\n", self.0.len());
        for (lugar, (n, mensaje, caso)) in &self.0 {
            informe += &format!("  {lugar} ×{n}: {mensaje}\n      primer caso: {caso}\n");
        }
        panic!("{informe}");
    }
}

/// Clase de paquete sintético.
#[derive(Clone, Copy, Debug)]
enum Clase {
    Aleatorio,
    TocValido,
    Perdida,
    Fec,
}

fn paquete_sintetico(g: &mut Generador) -> (Clase, Vec<u8>) {
    match g.hasta(10) {
        0..=3 => {
            let n = g.hasta(MAX_BYTES_ALEATORIOS + 1);
            (Clase::Aleatorio, g.bytes(n))
        }
        4..=7 => {
            // TOC (RFC 6716 §3.1): configuración (5 bits), estéreo (1) y código de tramas (2).
            let configuracion = g.hasta(32) as u8;
            let estereo = g.hasta(2) as u8;
            let codigo = g.hasta(4) as u8;
            let mut p = vec![(configuracion << 3) | (estereo << 2) | codigo];
            let n = g.hasta(MAX_CARGA_TRAS_TOC + 1);
            p.extend(g.bytes(n));
            (Clase::TocValido, p)
        }
        8 => (Clase::Perdida, Vec::new()),
        _ => {
            let n = g.hasta(MAX_CARGA_TRAS_TOC + 1);
            (Clase::Fec, g.bytes(n))
        }
    }
}

/// Longitud del búfer de salida: casi siempre la máxima, a veces corta.
fn longitud_salida(g: &mut Generador, canales: usize) -> usize {
    let maxima = MAX_MUESTRAS_POR_CANAL * canales;
    if g.hasta(8) == 0 {
        g.hasta(maxima + 1)
    } else {
        maxima
    }
}

#[test]
fn no_panico_paquetes_sinteticos() {
    instalar_gancho();
    let mut registro = Registro::default();
    let mut g = Generador(SEMILLA);

    for &frecuencia in &FRECUENCIAS {
        for canales in 1..=2 {
            let mut decodificador =
                OpusDecoder::new(frecuencia, canales).expect("configuración válida");
            for i in 0..PAQUETES_POR_CONFIGURACION {
                let (clase, paquete) = paquete_sintetico(&mut g);
                let fec = matches!(clase, Clase::Fec);
                let en_coma_flotante = g.hasta(2) == 0;
                let n = longitud_salida(&mut g, canales);
                let ok = registro.ejecutar(
                    || format!("{frecuencia} Hz, {canales} canal(es), paquete {i} ({clase:?}, {} bytes, float={en_coma_flotante}, salida={n}): {:02x?}", paquete.len(), &paquete[..paquete.len().min(16)]),
                    || {
                        if en_coma_flotante {
                            let mut pcm = vec![0f32; n];
                            let _ = decodificador.decode_float(&paquete, &mut pcm, fec);
                        } else {
                            let mut pcm = vec![0i16; n];
                            let _ = decodificador.decode(&paquete, &mut pcm, fec);
                        }
                    },
                );
                if !ok {
                    // El estado puede haber quedado a medias: se empieza con uno nuevo.
                    decodificador =
                        OpusDecoder::new(frecuencia, canales).expect("configuración válida");
                }
            }
        }
    }

    // Multiflujo: (frecuencia, canales, flujos, flujos acoplados, mapa de canales).
    let multiflujos: [(u32, usize, usize, usize, &[u8]); 3] = [
        (48_000, 2, 2, 0, &[0, 1]),
        (48_000, 3, 2, 1, &[0, 1, 2]),
        (16_000, 6, 4, 2, &[0, 4, 1, 2, 3, 5]),
    ];
    for &(frecuencia, canales, flujos, acoplados, mapa) in &multiflujos {
        let nuevo = || {
            OpusMultistreamDecoder::new(frecuencia, canales, flujos, acoplados, mapa)
                .expect("configuración válida")
        };
        let mut decodificador = nuevo();
        for i in 0..PAQUETES_POR_CONFIGURACION / 3 {
            let paquete = if g.hasta(2) == 0 {
                paquete_sintetico(&mut g).1
            } else {
                let subpaquetes: Vec<Vec<u8>> = (0..flujos)
                    .map(|_| {
                        let mut p = paquete_sintetico(&mut g).1;
                        // El prefijo autodelimitado solo admite tramas de hasta 1275 bytes.
                        p.truncate(MAX_BYTES_TRAMA);
                        p
                    })
                    .collect();
                let refs: Vec<&[u8]> = subpaquetes.iter().map(Vec::as_slice).collect();
                common::build_multistream_packet(&refs)
            };
            let fec = g.hasta(10) == 0;
            let en_coma_flotante = g.hasta(2) == 0;
            let n = longitud_salida(&mut g, canales);
            let ok = registro.ejecutar(
                || format!("multiflujo {frecuencia} Hz, {canales} canales, {flujos} flujos, paquete {i} ({} bytes, fec={fec}, float={en_coma_flotante}, salida={n})", paquete.len()),
                || {
                    if en_coma_flotante {
                        let mut pcm = vec![0f32; n];
                        let _ = decodificador.decode_float(&paquete, &mut pcm, fec);
                    } else {
                        let mut pcm = vec![0i16; n];
                        let _ = decodificador.decode(&paquete, &mut pcm, fec);
                    }
                },
            );
            if !ok {
                decodificador = nuevo();
            }
        }
    }

    registro.comprobar("no_panico_paquetes_sinteticos");
}

/// Paquetes híbridos de una trama, uno por configuración (12 a 15), cuya redundancia anunciada no
/// cabe en la trama. Se hallaron con una búsqueda aleatoria de 400 000 paquetes, en `release` y con
/// la comprobación de cordura desactivada a propósito: con ella quitada, cada uno hace entrar en
/// pánico al decodificador. El bit de redundancia de un paquete híbrido tiene probabilidad 1/4096
/// (RFC 6716 §4.5.1), así que el corpus aleatorio de las otras pruebas casi nunca llega a ese camino.
const PAQUETES_REDUNDANCIA_QUE_NO_CABE: [&[u8]; 4] = [
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

/// Regresión de la comprobación de cordura de la redundancia híbrida (`opus_decode_frame` en
/// libopus): cada paquete se decodifica con un decodificador recién creado.
#[test]
fn no_panico_redundancia_hibrida() {
    instalar_gancho();
    let mut registro = Registro::default();
    for (i, paquete) in PAQUETES_REDUNDANCIA_QUE_NO_CABE.iter().enumerate() {
        registro.ejecutar(
            || format!("paquete híbrido {i} (configuración {})", paquete[0] >> 3),
            || {
                let mut decodificador = OpusDecoder::new(48_000, 2).expect("configuración válida");
                let mut pcm = vec![0i16; MAX_MUESTRAS_POR_CANAL * 2];
                let _ = decodificador.decode(paquete, &mut pcm, false);
            },
        );
    }
    registro.comprobar("no_panico_redundancia_hibrida");
}

#[test]
#[ignore = "necesita los vectores de RFC 8251 en OPUS_TESTVECTORS_DIR"]
fn no_panico_vectores_mutados() {
    instalar_gancho();
    let mut registro = Registro::default();
    let mut g = Generador(SEMILLA ^ 0xA5A5_A5A5);

    for numero in 1..=12 {
        let nombre = format!("testvector{numero:02}");
        let especificacion = common::load_vector_spec(&nombre).expect("especificación del vector");
        let paquetes = common::load_packets(&nombre).expect("paquetes del vector");
        let canales = especificacion.channels;
        let frecuencia = especificacion.sample_rate;
        let mut decodificador =
            OpusDecoder::new(frecuencia, canales).expect("configuración válida");
        for (i, original) in paquetes.iter().enumerate() {
            let mut paquete = original.clone();
            let mutacion = g.hasta(4);
            match mutacion {
                0 => {}
                1 => paquete.truncate(g.hasta(paquete.len() + 1)),
                2 if !paquete.is_empty() => {
                    for _ in 0..=g.hasta(8) {
                        let bit = g.hasta(paquete.len() * 8);
                        paquete[bit / 8] ^= 1 << (bit % 8);
                    }
                }
                _ if !paquete.is_empty() => {
                    for _ in 0..=g.hasta(4) {
                        let j = g.hasta(paquete.len());
                        paquete[j] = g.siguiente() as u8;
                    }
                }
                _ => {}
            }
            let ok = registro.ejecutar(
                || {
                    format!(
                        "{nombre}, paquete {i}, mutación {mutacion}, {} bytes: {:02x?}",
                        paquete.len(),
                        &paquete[..paquete.len().min(16)]
                    )
                },
                || {
                    let mut pcm = vec![0i16; MAX_MUESTRAS_POR_CANAL * canales];
                    let _ = decodificador.decode(&paquete, &mut pcm, false);
                },
            );
            if !ok {
                decodificador =
                    OpusDecoder::new(frecuencia, canales).expect("configuración válida");
            }
        }
    }

    registro.comprobar("no_panico_vectores_mutados");
}

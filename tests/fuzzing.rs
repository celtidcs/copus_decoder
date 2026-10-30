//! Barrera de fuzzing del decodificador: secuencias de paquetes generados con la estructura de
//! RFC 6716 §3, y mutaciones de paquetes reales, en las que cada llamada se contrasta con unas
//! propiedades, no solo con la ausencia de pánico:
//!
//! - **P1.** Ninguna llamada entra en pánico (en el perfil de depuración, tampoco por
//!   desbordamiento aritmético).
//! - **P2.** Un paquete se rechaza con `InvalidPacket` si y solo si incumple alguna de las reglas
//!   R1–R7 de RFC 6716 §3.4 (en multiflujo, además, las de los subpaquetes autodelimitados del
//!   apéndice B y la misma duración en todos los flujos). Lo decide un oráculo escrito aquí,
//!   independiente del analizador del crate. Si se acepta, la llamada devuelve las muestras por
//!   canal que dice el oráculo: las del paquete o, al ocultar una pérdida o recuperarla con FEC,
//!   las del último paquete decodificado.
//! - **P3.** `BufferTooSmall` si y solo si la salida no cabe; tras un `Ok(n)`, nada se escribe más
//!   allá de `n` muestras por canal, y tras un error, nada en absoluto.
//! - **P4.** La misma secuencia da la misma salida, bit a bit, en dos decodificadores nuevos.
//! - **P5.** `decode_float` da exactamente `decode` / 32768.
//! - **P6.** Tras cualquier error, el decodificador sigue funcionando: el siguiente paquete, que
//!   se fuerza válido, se decodifica.
//!
//! Las secuencias salen de un generador con semilla fija, así que cada ejecución recorre los
//! mismos casos; cada fallo se informa con lo necesario para reproducirlo. La ronda corta corre
//! en la suite normal. La calibración del oráculo con los paquetes reales y la campaña larga, que
//! los muta, necesitan los vectores de RFC 8251 y las referencias de libopus 1.6.1 y están
//! marcadas `#[ignore]`; la campaña toma el número de pasos de `COPUS_FUZZ_ITERACIONES`.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use common::Generador;
use copus_decoder::{OpusDecoder, OpusError, OpusMultistreamDecoder};

/// Semillas de la ronda corta y de la campaña larga; fijas para que los casos sean reproducibles.
const SEMILLA_RONDA: u64 = 0xF022_0000_C0DE_6A01;
const SEMILLA_CAMPANA: u64 = 0xF022_0000_C0DE_6A02;
/// Pasos (llamadas a `decode` del primer decodificador) de la ronda corta: unos segundos en el
/// perfil de depuración.
const PASOS_RONDA: usize = 6_000;
/// Pasos de la campaña larga si no se define `COPUS_FUZZ_ITERACIONES`: lo bastante pocos para que
/// la regresión completa con `--include-ignored` no se alargue (un par de minutos en `release`).
/// La campaña de verdad, de 10⁷ pasos en `release` y 10⁶ en depuración, se lanza con la variable.
const ITERACIONES_CAMPANA: usize = 300_000;
/// Variable de entorno con el número de pasos de la campaña larga.
const VARIABLE_ITERACIONES: &str = "COPUS_FUZZ_ITERACIONES";
/// Pasos de cada secuencia, que empieza con decodificadores nuevos: de 1 a este número.
const MAX_PASOS_POR_SECUENCIA: usize = 24;

/// Uno de cada tantos paquetes de un flujo es uno de los híbridos cuya redundancia no cabe
/// (`common::PAQUETES_REDUNDANCIA_QUE_NO_CABE`), sin mutar: el bit de redundancia sale una vez de
/// cada 4096 tramas híbridas, así que el azar casi nunca llega a esa comprobación. Con 1 de 64,
/// la ronda corta los decodifica unas 70 veces.
const UNO_DE_REDUNDANCIA_QUE_NO_CABE: usize = 64;

/// Frecuencia de referencia de Opus: las duraciones del TOC se cuentan a 48 kHz (RFC 6716 §2).
const FRECUENCIA_OPUS_HZ: usize = 48_000;
/// Frecuencias de salida que admite el decodificador (RFC 6716 §2).
const FRECUENCIAS: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
/// Duración máxima de un paquete: 120 ms a 48 kHz (RFC 6716 §3.2.5).
const MAX_MUESTRAS_48K: usize = 5_760;
/// Bytes más largos que puede tener una trama (RFC 6716 §3.2.1, regla R2).
const MAX_BYTES_TRAMA: usize = 1_275;
/// Tramas más que puede declarar el byte de cuenta del código 3: sus 6 bits bajos.
const MAX_CUENTA_CODIGO3: usize = 63;
/// Un primer byte de longitud menor que este es la longitud entera; desde él, la longitud ocupa
/// dos bytes y vale `primero + 4 · segundo` (RFC 6716 §3.2.1).
const UMBRAL_LONGITUD_DOS_BYTES: usize = 252;
/// Un byte de relleno con este valor aporta `BYTES_RELLENO_CONTINUA` bytes y le sigue otro
/// (RFC 6716 §3.2.5).
const RELLENO_CONTINUA: u8 = 255;
const BYTES_RELLENO_CONTINUA: usize = 254;
/// Bits del byte de cuenta del código 3 (RFC 6716 §3.2.5): cuenta, relleno y tasa variable.
const CUENTA_MASCARA: u8 = 0x3f;
const CUENTA_RELLENO: u8 = 0x40;
const CUENTA_TASA_VARIABLE: u8 = 0x80;
/// Escala de `decode_float`: `i16::MIN` pasa a -1,0.
const ESCALA_FLOAT: f32 = 32_768.0;
/// Valores con que se llena la salida antes de cada llamada, para ver qué se ha escrito: el
/// flotante está fuera de `[-1, 1)`, así que el decodificador nunca lo escribe.
const CENTINELA_I16: i16 = 12_345;
const CENTINELA_F32: f32 = 7.0;

/// Topologías de multiflujo: (canales, flujos, acoplados, mapa). La última deja un canal en
/// silencio (255 en el mapa).
const MULTIFLUJOS: [(usize, usize, usize, &[u8]); 4] = [
    (2, 2, 0, &[0, 1]),
    (3, 2, 1, &[0, 1, 2]),
    (6, 4, 2, &[0, 4, 1, 2, 3, 5]),
    (4, 2, 1, &[0, 255, 1, 2]),
];

// ---------------------------------------------------------------------------------------------
// Oráculo: RFC 6716 §3 escrito de nuevo, sin mirar `src/packet.rs`.
// ---------------------------------------------------------------------------------------------

/// Regla de RFC 6716 §3.4 que incumple un paquete, para el informe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Regla {
    /// R1: un paquete tiene al menos un byte.
    R1,
    /// R2: ninguna trama de longitud implícita pasa de 1275 bytes.
    R2,
    /// R3: la carga del código 1 tiene una longitud par.
    R3,
    /// R4: el código 2 trae una longitud completa que cabe en lo que queda del paquete.
    R4,
    /// R5: el código 3 trae su byte de cuenta, con al menos una trama y 120 ms como mucho.
    R5,
    /// R6: en el código 3 de tasa constante, el relleno cabe y lo que queda se reparte por igual.
    R6,
    /// R7: en el código 3 de tasa variable, el relleno y las longitudes caben.
    R7,
    /// Apéndice B: falta la longitud autodelimitada o lo que declara se sale de los datos.
    Autodelimitado,
    /// Multiflujo: el paquete es más corto que `2 · flujos − 1`, falta un subpaquete o los
    /// subpaquetes no duran lo mismo.
    Multiflujo,
}

/// Forma de un paquete válido: cuántas tramas trae y cuánto dura cada una.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Forma {
    tramas: usize,
    muestras_48k_por_trama: usize,
}

impl Forma {
    fn muestras_48k(self) -> usize {
        self.tramas * self.muestras_48k_por_trama
    }
}

/// Muestras a 48 kHz de cada trama según la configuración del TOC (RFC 6716 §3.1, tabla 2): solo
/// SILK de 10, 20, 40 y 60 ms; híbrido de 10 y 20 ms; solo CELT de 2,5, 5, 10 y 20 ms.
fn muestras_48k_por_trama(toc: u8) -> usize {
    let configuracion = usize::from(toc >> 3);
    match configuracion {
        0..=11 => [480, 960, 1_920, 2_880][configuracion % 4],
        12..=15 => [480, 960][configuracion % 2],
        _ => [120, 240, 480, 960][configuracion % 4],
    }
}

/// Lee una longitud de trama de uno o dos bytes (RFC 6716 §3.2.1): `(longitud, bytes)`, o
/// `None` si faltan bytes.
fn leer_longitud(datos: &[u8]) -> Option<(usize, usize)> {
    let primero = usize::from(*datos.first()?);
    if primero < UMBRAL_LONGITUD_DOS_BYTES {
        return Some((primero, 1));
    }
    Some((primero + 4 * usize::from(*datos.get(1)?), 2))
}

/// Lee la longitud del relleno del código 3 (RFC 6716 §3.2.5): `(bytes de relleno, bytes
/// leídos)`, o `None` si faltan bytes.
fn leer_relleno(datos: &[u8]) -> Option<(usize, usize)> {
    let mut relleno = 0;
    for (leidos, &byte) in datos.iter().enumerate() {
        if byte != RELLENO_CONTINUA {
            return Some((relleno + usize::from(byte), leidos + 1));
        }
        relleno += BYTES_RELLENO_CONTINUA;
    }
    None
}

/// Cabecera de un paquete hasta las tramas: lo que dice de ellas antes de la última longitud.
struct Cabecera {
    forma: Forma,
    /// Bytes leídos: TOC, cuenta, relleno y longitudes explícitas.
    leidos: usize,
    /// Suma de las longitudes explícitas.
    explicitas: usize,
    /// Tramas cuya longitud es implícita (o, autodelimitado, la que da la longitud añadida): 2
    /// en el código 1, todas en el código 3 de tasa constante y 1 en el resto.
    implicitas: usize,
    /// Bytes de relleno tras las tramas.
    relleno: usize,
    /// Regla que se incumple si las tramas implícitas no caben o no se reparten bien.
    regla: Regla,
}

/// Lee la cabecera de `p` (RFC 6716 §3.2) sin mirar si las tramas caben.
fn leer_cabecera(p: &[u8]) -> Result<Cabecera, Regla> {
    let &toc = p.first().ok_or(Regla::R1)?;
    let muestras_48k_por_trama = muestras_48k_por_trama(toc);
    let cabecera = |tramas, leidos, explicitas, implicitas, relleno, regla| Cabecera {
        forma: Forma {
            tramas,
            muestras_48k_por_trama,
        },
        leidos,
        explicitas,
        implicitas,
        relleno,
        regla,
    };
    match toc & 0x03 {
        0 => Ok(cabecera(1, 1, 0, 1, 0, Regla::R2)),
        1 => Ok(cabecera(2, 1, 0, 2, 0, Regla::R3)),
        2 => {
            let (primera, usados) = leer_longitud(&p[1..]).ok_or(Regla::R4)?;
            Ok(cabecera(2, 1 + usados, primera, 1, 0, Regla::R4))
        }
        _ => {
            let &cuenta = p.get(1).ok_or(Regla::R5)?;
            let tramas = usize::from(cuenta & CUENTA_MASCARA);
            if tramas == 0 || tramas * muestras_48k_por_trama > MAX_MUESTRAS_48K {
                return Err(Regla::R5);
            }
            let variable = cuenta & CUENTA_TASA_VARIABLE != 0;
            let regla = if variable { Regla::R7 } else { Regla::R6 };
            let mut leidos = 2;
            let mut relleno = 0;
            if cuenta & CUENTA_RELLENO != 0 {
                let (bytes, usados) = leer_relleno(&p[leidos..]).ok_or(regla)?;
                relleno = bytes;
                leidos += usados;
            }
            if !variable {
                return Ok(cabecera(tramas, leidos, 0, tramas, relleno, regla));
            }
            let mut explicitas = 0;
            for _ in 1..tramas {
                let (longitud, usados) = leer_longitud(&p[leidos..]).ok_or(regla)?;
                explicitas += longitud;
                leidos += usados;
            }
            Ok(cabecera(tramas, leidos, explicitas, 1, relleno, regla))
        }
    }
}

/// Oráculo de un paquete en el formato normal (RFC 6716 §3.4, R1–R7): su forma, o la regla que
/// incumple.
fn oraculo(p: &[u8]) -> Result<Forma, Regla> {
    let c = leer_cabecera(p)?;
    let datos = (p.len() - c.leidos).checked_sub(c.relleno).ok_or(c.regla)?;
    let implicitos = datos.checked_sub(c.explicitas).ok_or(c.regla)?;
    if implicitos % c.implicitas != 0 {
        return Err(c.regla);
    }
    if implicitos / c.implicitas > MAX_BYTES_TRAMA {
        return Err(Regla::R2);
    }
    Ok(c.forma)
}

/// Oráculo de un subpaquete autodelimitado al principio de `datos` (RFC 6716, apéndice B): tras
/// la cabecera va la longitud de la última trama (de cada trama, en el código 1 y en el 3 de tasa
/// constante). Devuelve su forma y los bytes que ocupa, relleno incluido.
fn oraculo_autodelimitado(datos: &[u8]) -> Result<(Forma, usize), Regla> {
    let c = leer_cabecera(datos)?;
    let (longitud, usados) = leer_longitud(&datos[c.leidos..]).ok_or(Regla::Autodelimitado)?;
    let fin = c.leidos + usados + c.explicitas + longitud * c.implicitas + c.relleno;
    if fin > datos.len() {
        return Err(Regla::Autodelimitado);
    }
    Ok((c.forma, fin))
}

/// Oráculo de un paquete multiflujo de `flujos` flujos (RFC 7845 §5.1.1; troceo de
/// `opus_multistream_decode_native`): los `flujos − 1` primeros subpaquetes, autodelimitados; el
/// último, normal, ocupa el resto. Todos han de ser válidos y durar lo mismo. Devuelve las
/// muestras por canal a 48 kHz.
fn oraculo_multiflujo(p: &[u8], flujos: usize) -> Result<usize, Regla> {
    if p.len() < 2 * flujos - 1 {
        return Err(Regla::Multiflujo);
    }
    let mut resto = p;
    let mut duracion = None;
    for flujo in 0..flujos {
        if resto.is_empty() {
            return Err(Regla::Multiflujo);
        }
        let forma = if flujo + 1 < flujos {
            let (forma, ocupa) = oraculo_autodelimitado(resto)?;
            resto = &resto[ocupa..];
            forma
        } else {
            oraculo(resto)?
        };
        if duracion.is_some_and(|d| d != forma.muestras_48k()) {
            return Err(Regla::Multiflujo);
        }
        duracion = Some(forma.muestras_48k());
    }
    Ok(duracion.unwrap_or(0))
}

// ---------------------------------------------------------------------------------------------
// Modelo de lo que ha de devolver cada llamada.
// ---------------------------------------------------------------------------------------------

/// Resultado de una llamada, comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Salida {
    Muestras(usize),
    PaqueteInvalido,
    BufferPequeno,
    /// `InternalError` o `InvalidArgument`: no deben salir nunca de una llamada a `decode`.
    Inesperado,
}

impl Salida {
    /// Nombre de la variante, sin las muestras, para agrupar los fallos.
    fn clase(self) -> &'static str {
        match self {
            Self::Muestras(_) => "Muestras",
            Self::PaqueteInvalido => "PaqueteInvalido",
            Self::BufferPequeno => "BufferPequeno",
            Self::Inesperado => "Inesperado",
        }
    }

    fn de(resultado: Result<usize, OpusError>) -> Self {
        match resultado {
            Ok(n) => Self::Muestras(n),
            Err(OpusError::InvalidPacket) => Self::PaqueteInvalido,
            Err(OpusError::BufferTooSmall) => Self::BufferPequeno,
            Err(_) => Self::Inesperado,
        }
    }
}

/// Topología del decodificador que se prueba.
#[derive(Debug, Clone, Copy)]
enum Topologia {
    Flujo { frecuencia: u32, canales: usize },
    Multiflujo { frecuencia: u32, indice: usize },
}

impl Topologia {
    fn frecuencia(self) -> u32 {
        match self {
            Self::Flujo { frecuencia, .. } | Self::Multiflujo { frecuencia, .. } => frecuencia,
        }
    }

    fn canales(self) -> usize {
        match self {
            Self::Flujo { canales, .. } => canales,
            Self::Multiflujo { indice, .. } => MULTIFLUJOS[indice].0,
        }
    }

    fn flujos(self) -> usize {
        match self {
            Self::Flujo { .. } => 1,
            Self::Multiflujo { indice, .. } => MULTIFLUJOS[indice].1,
        }
    }

    /// Muestras por canal a 48 kHz del paquete según el oráculo, o la regla que incumple.
    fn oraculo(self, paquete: &[u8]) -> Result<usize, Regla> {
        match self {
            Self::Flujo { .. } => oraculo(paquete).map(Forma::muestras_48k),
            Self::Multiflujo { .. } => oraculo_multiflujo(paquete, self.flujos()),
        }
    }

    fn muestras_maximas(self) -> usize {
        MAX_MUESTRAS_48K * self.frecuencia() as usize / FRECUENCIA_OPUS_HZ * self.canales()
    }
}

/// Lo que se espera de cada llamada: el único estado que importa es la duración del último
/// paquete decodificado, que es la de la ocultación y la de la recuperación con FEC.
struct Modelo {
    topologia: Topologia,
    ultima: usize,
}

impl Modelo {
    fn prever(&self, paquete: &[u8], fec: bool, longitud: usize) -> Salida {
        let muestras = if paquete.is_empty() {
            self.ultima
        } else {
            match self.topologia.oraculo(paquete) {
                Err(_) => return Salida::PaqueteInvalido,
                Ok(_) if fec => self.ultima,
                Ok(m48) => m48 * self.topologia.frecuencia() as usize / FRECUENCIA_OPUS_HZ,
            }
        };
        if longitud < muestras * self.topologia.canales() {
            Salida::BufferPequeno
        } else {
            Salida::Muestras(muestras)
        }
    }

    fn registrar(&mut self, paquete: &[u8], fec: bool, salida: Salida) {
        if let Salida::Muestras(n) = salida {
            if !paquete.is_empty() && !fec {
                self.ultima = n;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Decodificadores bajo prueba.
// ---------------------------------------------------------------------------------------------

enum Decodificador {
    Flujo(Box<OpusDecoder>),
    Multiflujo(OpusMultistreamDecoder),
}

impl Decodificador {
    fn nuevo(topologia: Topologia) -> Self {
        match topologia {
            Topologia::Flujo {
                frecuencia,
                canales,
            } => Self::Flujo(Box::new(
                OpusDecoder::new(frecuencia, canales).expect("configuración válida"),
            )),
            Topologia::Multiflujo { frecuencia, indice } => {
                let (canales, flujos, acoplados, mapa) = MULTIFLUJOS[indice];
                Self::Multiflujo(
                    OpusMultistreamDecoder::new(frecuencia, canales, flujos, acoplados, mapa)
                        .expect("configuración válida"),
                )
            }
        }
    }

    fn decode(&mut self, paquete: &[u8], pcm: &mut [i16], fec: bool) -> Salida {
        Salida::de(match self {
            Self::Flujo(d) => d.decode(paquete, pcm, fec),
            Self::Multiflujo(d) => d.decode(paquete, pcm, fec),
        })
    }

    fn decode_float(&mut self, paquete: &[u8], pcm: &mut [f32], fec: bool) -> Salida {
        Salida::de(match self {
            Self::Flujo(d) => d.decode_float(paquete, pcm, fec),
            Self::Multiflujo(d) => d.decode_float(paquete, pcm, fec),
        })
    }
}

/// Ejecuta `f` y convierte un pánico en `Err` con su mensaje (P1).
fn sin_panico<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(f)).map_err(|e| {
        e.downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_default()
    })
}

// ---------------------------------------------------------------------------------------------
// Generación de paquetes.
// ---------------------------------------------------------------------------------------------

/// Codifica la longitud de un relleno de `bytes` bytes (RFC 6716 §3.2.5).
fn codificar_relleno(mut bytes: usize) -> Vec<u8> {
    let mut v = Vec::new();
    while bytes >= usize::from(RELLENO_CONTINUA) {
        v.push(RELLENO_CONTINUA);
        bytes -= BYTES_RELLENO_CONTINUA;
    }
    v.push(u8::try_from(bytes).expect("menos de 255"));
    v
}

/// Longitud de una trama, cargada hacia los límites: vacía, de un byte (DTX), alrededor del
/// umbral de dos bytes y alrededor de 1275.
fn longitud_trama(g: &mut Generador, pequena: bool) -> usize {
    match g.hasta(16) {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => UMBRAL_LONGITUD_DOS_BYTES - 1 + g.hasta(3),
        4 if !pequena => MAX_BYTES_TRAMA - 2 + g.hasta(5),
        5 | 6 if !pequena => g.hasta(MAX_BYTES_TRAMA + 1),
        _ => 3 + g.hasta(120),
    }
}

/// Contenido de una trama de `n` bytes: casi siempre al azar; a veces, todo ceros o todo unos.
fn contenido(g: &mut Generador, n: usize) -> Vec<u8> {
    match g.hasta(10) {
        0 => vec![0; n],
        1 => vec![0xff; n],
        _ => g.bytes(n),
    }
}

/// Número de tramas del código 3, cargado hacia los límites de R5.
fn cuenta_codigo3(g: &mut Generador, por_trama: usize) -> usize {
    let maxima = MAX_MUESTRAS_48K / por_trama;
    match g.hasta(8) {
        0 => 0,
        1 => 1,
        2 => maxima,
        3 => maxima + 1,
        4 => MAX_CUENTA_CODIGO3 - g.hasta(16),
        _ => 1 + g.hasta(maxima),
    }
}

/// Bytes de relleno del código 3, alrededor de los límites de su codificación.
fn relleno(g: &mut Generador) -> usize {
    match g.hasta(6) {
        0 => 0,
        1 => 1 + g.hasta(10),
        2 => 253 + g.hasta(4),
        3 => 507 + g.hasta(4),
        _ => g.hasta(64),
    }
}

/// Paquete construido según RFC 6716 §3.2, con cualquier TOC: casi siempre válido, salvo en los
/// límites que se eligen a propósito (más de 1275 bytes, más de 120 ms, cero tramas).
fn paquete_estructurado(g: &mut Generador, toc: u8) -> Vec<u8> {
    let mut p = vec![toc];
    match toc & 0x03 {
        0 => {
            let n = longitud_trama(g, false);
            p.extend(contenido(g, n));
        }
        1 => {
            let n = longitud_trama(g, false);
            p.extend(contenido(g, 2 * n));
        }
        2 => {
            let (a, b) = (longitud_trama(g, false), longitud_trama(g, false));
            p.extend(common::encode_frame_size(a.min(MAX_BYTES_TRAMA)));
            p.extend(contenido(g, a + b));
        }
        _ => {
            let tramas = cuenta_codigo3(g, muestras_48k_por_trama(toc));
            let variable = g.hasta(2) == 0;
            let bytes_relleno = relleno(g);
            let mut cuenta = u8::try_from(tramas.min(MAX_CUENTA_CODIGO3)).expect("6 bits");
            if variable {
                cuenta |= CUENTA_TASA_VARIABLE;
            }
            if bytes_relleno > 0 || g.hasta(4) == 0 {
                cuenta |= CUENTA_RELLENO;
            }
            p.push(cuenta);
            if cuenta & CUENTA_RELLENO != 0 {
                p.extend(codificar_relleno(bytes_relleno));
            }
            let pequena = tramas > 4;
            let longitudes: Vec<usize> = if variable {
                (0..tramas.max(1))
                    .map(|_| longitud_trama(g, pequena))
                    .collect()
            } else {
                vec![longitud_trama(g, pequena); tramas.max(1)]
            };
            if variable {
                for &n in &longitudes[..longitudes.len() - 1] {
                    p.extend(common::encode_frame_size(n.min(MAX_BYTES_TRAMA)));
                }
            }
            let total: usize = longitudes.iter().sum();
            p.extend(contenido(g, total));
            if cuenta & CUENTA_RELLENO != 0 {
                p.extend(g.bytes(bytes_relleno));
            }
        }
    }
    p
}

/// TOC al azar: configuración (5 bits), estéreo (1) y código de tramas (2) (RFC 6716 §3.1).
fn toc_al_azar(g: &mut Generador) -> u8 {
    g.siguiente() as u8
}

/// Mutación de la estructura de `p`: cortar, alargar, cambiar un byte de la cabecera, invertir
/// un bit o quitar un byte de la cabecera. Deja casos justo a un lado y otro de cada regla.
fn mutar(g: &mut Generador, p: &mut Vec<u8>) {
    let cabecera = p.len().min(6);
    match g.hasta(6) {
        0 => p.truncate(g.hasta(p.len() + 1)),
        1 => {
            let n = 1 + g.hasta(3);
            p.extend(g.bytes(n));
        }
        2 if cabecera > 0 => {
            let i = g.hasta(cabecera);
            p[i] = g.siguiente() as u8;
        }
        3 if cabecera > 0 => {
            let i = g.hasta(cabecera);
            p[i] = p[i].wrapping_add(if g.hasta(2) == 0 { 1 } else { 255 });
        }
        4 if !p.is_empty() => {
            let bit = g.hasta(p.len() * 8);
            p[bit / 8] ^= 1 << (bit % 8);
        }
        _ if cabecera > 0 => {
            p.remove(g.hasta(cabecera));
        }
        _ => {}
    }
}

/// Paquetes reales de los que parte la campaña larga: en ellos, la mutación toca contenido de
/// verdad, no bytes al azar.
struct Corpus {
    flujos: Vec<Vec<u8>>,
    multiflujos: Vec<Vec<u8>>,
}

/// Paquete de un flujo para el paso siguiente.
fn paquete_flujo(g: &mut Generador, corpus: Option<&Corpus>) -> Vec<u8> {
    if g.hasta(UNO_DE_REDUNDANCIA_QUE_NO_CABE) == 0 {
        let paquetes = common::PAQUETES_REDUNDANCIA_QUE_NO_CABE;
        return paquetes[g.hasta(paquetes.len())].to_vec();
    }
    let mut p = match corpus {
        Some(c) if g.hasta(2) == 0 => {
            let mut p = c.flujos[g.hasta(c.flujos.len())].clone();
            if g.hasta(4) == 0 && !p.is_empty() {
                // Otro TOC sobre el mismo contenido: otro modo, otra duración u otro código.
                p[0] = toc_al_azar(g);
            }
            p
        }
        _ => {
            let toc = toc_al_azar(g);
            paquete_estructurado(g, toc)
        }
    };
    if g.hasta(3) == 0 {
        mutar(g, &mut p);
    }
    p
}

/// Paquete multiflujo de `flujos` flujos para el paso siguiente: casi siempre, todos los
/// subpaquetes con la misma duración.
fn paquete_multiflujo(g: &mut Generador, flujos: usize, corpus: Option<&Corpus>) -> Vec<u8> {
    let mut p = match corpus {
        Some(c) if flujos == 2 && g.hasta(2) == 0 => {
            c.multiflujos[g.hasta(c.multiflujos.len())].clone()
        }
        _ => {
            let primero = toc_al_azar(g);
            let subpaquetes: Vec<Vec<u8>> = (0..flujos)
                .map(|_| {
                    let toc = if g.hasta(8) == 0 {
                        toc_al_azar(g)
                    } else {
                        // Mismo TOC salvo el bit estéreo: la misma duración.
                        (primero & !0x04) | ((g.siguiente() as u8) & 0x04)
                    };
                    paquete_estructurado(g, toc)
                })
                .collect();
            let refs: Vec<&[u8]> = subpaquetes.iter().map(Vec::as_slice).collect();
            common::build_multistream_packet(&refs)
        }
    };
    if g.hasta(3) == 0 {
        mutar(g, &mut p);
    }
    p
}

// ---------------------------------------------------------------------------------------------
// Ejecución de las secuencias.
// ---------------------------------------------------------------------------------------------

/// Fallos de una ronda, por clase (propiedad, resultado esperado y obtenido, regla, código y
/// tamaño): cuántos y el primero, con lo necesario para reproducirlo.
#[derive(Default)]
struct Informe {
    clases: BTreeMap<String, (usize, String)>,
    total: usize,
    pasos: usize,
    validos: usize,
    invalidos: usize,
}

impl Informe {
    fn fallo(&mut self, clase: String, texto: String) {
        self.total += 1;
        self.clases.entry(clase).or_insert((0, texto)).0 += 1;
    }

    fn unir(&mut self, otro: Informe) {
        self.total += otro.total;
        self.pasos += otro.pasos;
        self.validos += otro.validos;
        self.invalidos += otro.invalidos;
        for (clase, (n, texto)) in otro.clases {
            self.clases.entry(clase).or_insert((0, texto)).0 += n;
        }
    }

    fn comprobar(&self, prueba: &str) {
        println!(
            "{prueba}: {} pasos ({} paquetes válidos y {} inválidos según el oráculo), {} fallos",
            self.pasos, self.validos, self.invalidos, self.total
        );
        if self.total == 0 {
            return;
        }
        let clases: Vec<String> = self
            .clases
            .iter()
            .map(|(clase, (n, texto))| {
                format!(
                    "  {n:6} × {clase}
         p. ej. {texto}"
                )
            })
            .collect();
        panic!(
            "{prueba}: {} fallos en {} pasos, por clase:
{}",
            self.total,
            self.pasos,
            clases.join(
                "
"
            )
        );
    }
}

/// Elige la topología de una secuencia: las 10 configuraciones de salida de un flujo y los
/// multiflujos.
fn topologia(g: &mut Generador) -> Topologia {
    let frecuencia = FRECUENCIAS[g.hasta(FRECUENCIAS.len())];
    if g.hasta(4) == 0 {
        Topologia::Multiflujo {
            frecuencia,
            indice: g.hasta(MULTIFLUJOS.len()),
        }
    } else {
        Topologia::Flujo {
            frecuencia,
            canales: 1 + g.hasta(2),
        }
    }
}

/// Longitud de la salida: casi siempre la máxima; a veces la justa, una muestra menos o al azar,
/// para recorrer el límite de P3.
fn longitud_salida(g: &mut Generador, prevista: Salida, modelo: &Modelo) -> usize {
    let maxima = modelo.topologia.muestras_maximas();
    let justa = match prevista {
        Salida::Muestras(n) => n * modelo.topologia.canales(),
        _ => maxima,
    };
    match g.hasta(8) {
        0 => justa,
        1 => justa.saturating_sub(1),
        2 => g.hasta(maxima + 1),
        _ => maxima,
    }
}

/// Primeros bytes de un paquete, para el informe.
fn resumen(p: &[u8]) -> String {
    format!("{} bytes {:02x?}", p.len(), &p[..p.len().min(24)])
}

/// Generador, topología y número de pasos de la secuencia `indice` de una ronda con semilla
/// `semilla`: lo primero que sale de su generador.
fn inicio_de_secuencia(semilla: u64, indice: usize) -> (Generador, Topologia, usize) {
    let mut g = Generador(semilla ^ (indice as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let topologia = topologia(&mut g);
    let pasos = 1 + g.hasta(MAX_PASOS_POR_SECUENCIA);
    (g, topologia, pasos)
}

/// Ejecuta la secuencia `indice` de una ronda con semilla `semilla` y anota sus fallos.
fn secuencia(semilla: u64, indice: usize, corpus: Option<&Corpus>, informe: &mut Informe) {
    let (mut g, topologia, pasos) = inicio_de_secuencia(semilla, indice);
    let canales = topologia.canales();
    let mut modelo = Modelo {
        topologia,
        ultima: 0,
    };
    let mut a = Decodificador::nuevo(topologia);
    let mut b = Decodificador::nuevo(topologia);
    let mut c = Decodificador::nuevo(topologia);
    let mut tras_error = false;

    for paso in 0..pasos {
        // P6: tras un error, el paquete siguiente es válido y la salida, la máxima.
        let (paquete, fec) = loop {
            let paquete = match g.hasta(10) {
                0 if !tras_error => Vec::new(),
                _ => match topologia {
                    Topologia::Flujo { .. } => paquete_flujo(&mut g, corpus),
                    Topologia::Multiflujo { .. } => {
                        paquete_multiflujo(&mut g, topologia.flujos(), corpus)
                    }
                },
            };
            let fec = !tras_error && g.hasta(8) == 0;
            if !tras_error || topologia.oraculo(&paquete).is_ok() {
                break (paquete, fec);
            }
        };
        let previa = modelo.prever(&paquete, fec, usize::MAX);
        let longitud = if tras_error {
            topologia.muestras_maximas()
        } else {
            longitud_salida(&mut g, previa, &modelo)
        };
        let esperada = modelo.prever(&paquete, fec, longitud);
        let caso = || {
            format!(
                "secuencia {indice} (semilla {semilla:#x}), paso {paso}, {topologia:?}, fec={fec}, salida={longitud}, paquete {}",
                resumen(&paquete)
            )
        };

        let mut pcm_a = vec![CENTINELA_I16; longitud];
        let mut pcm_b = vec![CENTINELA_I16; longitud];
        let mut pcm_c = vec![CENTINELA_F32; longitud];
        let resultado = sin_panico(|| {
            (
                a.decode(&paquete, &mut pcm_a, fec),
                b.decode(&paquete, &mut pcm_b, fec),
                c.decode_float(&paquete, &mut pcm_c, fec),
            )
        });
        let (sa, sb, sc) = match resultado {
            Ok(salidas) => salidas,
            Err(mensaje) => {
                let clase = format!("P1 pánico «{}»", mensaje.lines().next().unwrap_or(""));
                informe.fallo(clase, format!("P1 pánico «{mensaje}»: {}", caso()));
                return;
            }
        };
        informe.pasos += 1;
        if topologia.oraculo(&paquete).is_ok() || paquete.is_empty() {
            informe.validos += 1;
        } else {
            informe.invalidos += 1;
        }

        if sa != esperada {
            let regla = topologia.oraculo(&paquete).err();
            let p = if matches!(esperada, Salida::BufferPequeno)
                || matches!(sa, Salida::BufferPequeno)
            {
                "P3"
            } else {
                "P2"
            };
            let clase = format!(
                "{p} {} → {}, regla {regla:?}, código {:?}, {}",
                esperada.clase(),
                sa.clase(),
                paquete.first().map(|toc| toc & 0x03),
                if paquete.len() > MAX_BYTES_TRAMA {
                    "> 1275 bytes"
                } else {
                    "≤ 1275 bytes"
                }
            );
            informe.fallo(
                clase,
                format!(
                    "{p} esperado {esperada:?} (regla {regla:?}), obtenido {sa:?}: {}",
                    caso()
                ),
            );
            return;
        }
        let escritas = match sa {
            Salida::Muestras(n) => n * canales,
            _ => 0,
        };
        if pcm_a[escritas..].iter().any(|&s| s != CENTINELA_I16)
            || pcm_c[escritas..]
                .iter()
                .any(|s| s.to_bits() != CENTINELA_F32.to_bits())
        {
            informe.fallo(
                "P3 escribe más allá".into(),
                format!("P3 escribe más allá de {escritas} muestras: {}", caso()),
            );
            return;
        }
        if sb != sa || pcm_b != pcm_a {
            informe.fallo(
                "P4".into(),
                format!("P4 {sa:?} frente a {sb:?}: {}", caso()),
            );
            return;
        }
        let distinta = pcm_a[..escritas]
            .iter()
            .zip(&pcm_c[..escritas])
            .position(|(&i, &f)| (f32::from(i) / ESCALA_FLOAT).to_bits() != f.to_bits());
        if sc != sa || distinta.is_some() {
            informe.fallo(
                format!("P5 {} → {}", sa.clase(), sc.clase()),
                format!(
                    "P5 {sa:?} frente a {sc:?}, primera muestra distinta {distinta:?}: {}",
                    caso()
                ),
            );
            return;
        }
        if tras_error && !matches!(sa, Salida::Muestras(_)) {
            informe.fallo(
                "P6".into(),
                format!("P6 no se recupera: {sa:?}: {}", caso()),
            );
            return;
        }
        modelo.registrar(&paquete, fec, sa);
        tras_error = !matches!(sa, Salida::Muestras(_));
    }
}

/// Ejecuta secuencias hasta sumar `pasos` pasos, repartidas entre los hilos disponibles; cada
/// secuencia depende solo de la semilla y de su índice, así que el resultado no depende del
/// reparto.
fn ronda(semilla: u64, pasos: usize, corpus: Option<&Corpus>) -> Informe {
    let hilos = std::thread::available_parallelism().map_or(1, usize::from);
    // Las secuencias necesarias para sumar al menos `pasos` pasos.
    let mut secuencias = 0;
    let mut suma = 0;
    while suma < pasos {
        suma += inicio_de_secuencia(semilla, secuencias).2;
        secuencias += 1;
    }
    let mut total = Informe::default();
    std::thread::scope(|s| {
        let trabajos: Vec<_> = (0..hilos)
            .map(|h| {
                s.spawn(move || {
                    let mut informe = Informe::default();
                    for indice in (h..secuencias).step_by(hilos) {
                        secuencia(semilla, indice, corpus, &mut informe);
                    }
                    informe
                })
            })
            .collect();
        for trabajo in trabajos {
            total.unir(trabajo.join().expect("el arnés no entra en pánico"));
        }
    });
    total
}

#[test]
fn ronda_corta_de_fuzzing() {
    ronda(SEMILLA_RONDA, PASOS_RONDA, None).comprobar("ronda_corta_de_fuzzing");
}

/// Paquetes reales: los de los vectores de RFC 8251 y los de las referencias de libopus 1.6.1 (de
/// un flujo y multiflujo), sin los registros de pérdida.
fn corpus_real() -> Corpus {
    let mut flujos = Vec::new();
    for numero in 1..=12 {
        flujos.extend(
            common::load_packets(&format!("testvector{numero:02}"))
                .expect("vectores de RFC 8251 en OPUS_TESTVECTORS_DIR"),
        );
    }
    let dir = common::reference_dir();
    flujos.extend(paquetes_de_carpeta(&dir.join("flujos")));
    let multiflujos = paquetes_de_carpeta(&dir.join("multiflujo"));
    Corpus {
        flujos,
        multiflujos,
    }
}

/// Paquetes de todos los `.bit` de `carpeta`, sin los registros de pérdida.
fn paquetes_de_carpeta(carpeta: &Path) -> Vec<Vec<u8>> {
    let mut rutas: Vec<_> = fs::read_dir(carpeta)
        .unwrap_or_else(|e| panic!("no se puede leer {}: {e}", carpeta.display()))
        .map(|e| e.expect("entrada de la carpeta").path())
        .filter(|r| r.extension().is_some_and(|x| x == "bit"))
        .collect();
    rutas.sort();
    rutas
        .iter()
        .flat_map(|r| common::read_opus_demo_stream(r).expect("flujo de la referencia"))
        .flatten()
        .collect()
}

#[test]
#[ignore = "necesita los vectores de RFC 8251 y las referencias de libopus 1.6.1; pasos en COPUS_FUZZ_ITERACIONES"]
fn campana_larga_de_fuzzing() {
    let pasos = std::env::var(VARIABLE_ITERACIONES)
        .ok()
        .map_or(ITERACIONES_CAMPANA, |v| {
            v.parse().expect("COPUS_FUZZ_ITERACIONES es un número")
        });
    let corpus = corpus_real();
    let inicio = std::time::Instant::now();
    let informe = ronda(SEMILLA_CAMPANA, pasos, Some(&corpus));
    println!(
        "campana_larga_de_fuzzing: semilla {SEMILLA_CAMPANA:#x}, {:.1} s",
        inicio.elapsed().as_secs_f64()
    );
    informe.comprobar("campana_larga_de_fuzzing");
}

/// Calibración del oráculo (§1.7): todos los paquetes reales son válidos, y la suma de sus
/// duraciones es la longitud de la salida de libopus en los flujos sin pérdidas.
#[test]
#[ignore = "necesita los vectores de RFC 8251 y las referencias de libopus 1.6.1"]
fn el_oraculo_acepta_los_paquetes_reales() {
    let mut fallos = Vec::new();
    for numero in 1..=12 {
        let nombre = format!("testvector{numero:02}");
        let paquetes = common::load_packets(&nombre).expect("vectores de RFC 8251");
        let salida = common::vectors_dir().join(format!("{nombre}.dec"));
        // Salida de libopus a 48 kHz en estéreo, `i16`.
        comprobar_flujo(&nombre, &paquetes, 1, Some((&salida, 2)), &mut fallos);
    }
    let dir = common::reference_dir();
    for paquetes_de in ["flujos", "multiflujo"] {
        let mut rutas: Vec<_> = fs::read_dir(dir.join(paquetes_de))
            .expect("carpeta de la referencia")
            .map(|e| e.expect("entrada").path())
            .filter(|r| r.extension().is_some_and(|x| x == "bit"))
            .collect();
        rutas.sort();
        for ruta in rutas {
            let nombre = ruta.file_stem().unwrap().to_string_lossy().into_owned();
            let registros = common::read_opus_demo_stream(&ruta).expect("flujo de la referencia");
            let sin_perdidas = registros.iter().all(Option::is_some);
            let paquetes: Vec<Vec<u8>> = registros.into_iter().flatten().collect();
            let (flujos, canales) = if paquetes_de == "multiflujo" {
                (2, 3)
            } else {
                (1, 2)
            };
            let salida = if paquetes_de == "multiflujo" {
                ruta.with_extension("dec")
            } else {
                dir.join("salidas").join(format!("{nombre}.dec"))
            };
            let referencia =
                (sin_perdidas && salida.exists()).then_some((salida.as_path(), canales));
            comprobar_flujo(&nombre, &paquetes, flujos, referencia, &mut fallos);
        }
    }
    assert!(
        fallos.is_empty(),
        "calibración del oráculo:\n{}",
        fallos.join("\n")
    );
}

/// Comprueba que el oráculo acepta todos los `paquetes` de `nombre` y, si hay `referencia`
/// (salida de libopus `i16` a 48 kHz con esos canales), que su duración total es la de ella.
fn comprobar_flujo(
    nombre: &str,
    paquetes: &[Vec<u8>],
    flujos: usize,
    referencia: Option<(&Path, usize)>,
    fallos: &mut Vec<String>,
) {
    let mut total = 0usize;
    for (i, p) in paquetes.iter().enumerate() {
        let forma = if flujos == 1 {
            oraculo(p).map(Forma::muestras_48k)
        } else {
            oraculo_multiflujo(p, flujos)
        };
        match forma {
            Ok(m) => total += m,
            Err(regla) => fallos.push(format!("{nombre}, paquete {i}: {regla:?}, {}", resumen(p))),
        }
    }
    let Some((salida, canales)) = referencia else {
        println!(
            "{nombre}: {} paquetes válidos, {total} muestras",
            paquetes.len()
        );
        return;
    };
    let bytes = fs::metadata(salida).expect("salida de libopus").len() as usize;
    let muestras = bytes / (2 * canales);
    println!(
        "{nombre}: {} paquetes válidos, {total} muestras; libopus, {muestras}",
        paquetes.len()
    );
    if muestras != total {
        fallos.push(format!(
            "{nombre}: el oráculo suma {total} muestras y libopus da {muestras}"
        ));
    }
}

// ---------------------------------------------------------------------------------------------
// Regresiones de los defectos que encontró el fuzzing.
// ---------------------------------------------------------------------------------------------

/// TOC de las tramas solo CELT de banda completa de 2,5 ms (configuración 28), 5 ms (29) y 20 ms
/// (31), mono y con el código 0; el código de tramas va en los dos bits bajos (RFC 6716 §3.1).
const TOC_CELT_2_5_MS: u8 = 28 << 3;
const TOC_CELT_5_MS: u8 = 29 << 3;
const TOC_CELT_20_MS: u8 = 31 << 3;
/// TOC de una trama solo SILK de banda estrecha de 20 ms (configuración 1), mono.
const TOC_SILK_20_MS: u8 = 1 << 3;
/// Muestras a 48 kHz de esas tramas.
const MUESTRAS_2_5_MS: usize = 120;
const MUESTRAS_20_MS: usize = 960;
/// Códigos de tramas del TOC (RFC 6716 §3.2).
const CODIGO_1: u8 = 1;
const CODIGO_2: u8 = 2;
const CODIGO_3: u8 = 3;

/// Decodifica `paquete` con un decodificador nuevo a 48 kHz en estéreo y la salida máxima.
fn decodificar(paquete: &[u8]) -> Result<usize, OpusError> {
    let mut decodificador = OpusDecoder::new(48_000, 2).expect("configuración válida");
    let mut pcm = vec![0i16; MAX_MUESTRAS_48K * 2];
    decodificador.decode(paquete, &mut pcm, false)
}

/// Paquete con la cabecera `cabecera` seguida de `n` bytes de carga.
fn con_carga(cabecera: &[u8], n: usize) -> Vec<u8> {
    let mut p = cabecera.to_vec();
    p.resize(cabecera.len() + n, 0x5a);
    p
}
/// R2 (RFC 6716 §3.4; `opus.c:363`): ninguna trama de longitud implícita pasa de 1275 bytes.
#[test]
fn una_trama_implicita_de_mas_de_1275_bytes_no_es_valida() {
    let largo = MAX_BYTES_TRAMA + 1;
    let invalidos = [
        // Código 0: la trama entera.
        con_carga(&[TOC_CELT_20_MS], largo),
        // Código 1: cada una de las dos.
        con_carga(&[TOC_CELT_20_MS | CODIGO_1], 2 * largo),
        // Código 2: la segunda, tras una primera vacía.
        con_carga(&[TOC_CELT_20_MS | CODIGO_2, 0], largo),
        // Código 3 de tasa constante, una trama.
        con_carga(&[TOC_CELT_20_MS | CODIGO_3, 1], largo),
        // Código 3 de tasa variable, dos tramas: la última, tras una primera vacía.
        con_carga(
            &[TOC_CELT_20_MS | CODIGO_3, CUENTA_TASA_VARIABLE | 2, 0],
            largo,
        ),
    ];
    for p in &invalidos {
        assert!(
            matches!(decodificar(p), Err(OpusError::InvalidPacket)),
            "se acepta {}",
            resumen(p)
        );
    }
    assert_eq!(
        decodificar(&con_carga(&[TOC_CELT_20_MS], MAX_BYTES_TRAMA)).ok(),
        Some(MUESTRAS_20_MS)
    );
    assert_eq!(
        decodificar(&con_carga(&[TOC_CELT_20_MS | CODIGO_2, 0], MAX_BYTES_TRAMA)).ok(),
        Some(2 * MUESTRAS_20_MS)
    );
}
/// Ni RFC 6716 ni libopus limitan la longitud de un paquete: solo sus tramas (1275 bytes) y su
/// duración (120 ms). El relleno del código 3 tampoco tiene límite.
#[test]
fn un_paquete_valido_de_mas_de_1500_bytes_se_decodifica() {
    let tramas_2_5_ms = MAX_MUESTRAS_48K / MUESTRAS_2_5_MS;
    let cuenta = u8::try_from(tramas_2_5_ms).expect("48 tramas");
    let casos = [
        // Código 1: dos tramas de 1275 bytes.
        (
            con_carga(&[TOC_CELT_20_MS | CODIGO_1], 2 * MAX_BYTES_TRAMA),
            2 * MUESTRAS_20_MS,
        ),
        // Código 3 de tasa constante: 48 tramas de 2,5 ms de 1275 bytes (61 202 bytes).
        (
            con_carga(
                &[TOC_CELT_2_5_MS | CODIGO_3, cuenta],
                tramas_2_5_ms * MAX_BYTES_TRAMA,
            ),
            MAX_MUESTRAS_48K,
        ),
        // Código 3 con una trama de 100 bytes y 2000 de relleno.
        (
            {
                let mut cabecera = vec![TOC_CELT_20_MS | CODIGO_3, CUENTA_RELLENO | 1];
                cabecera.extend(codificar_relleno(2_000));
                con_carga(&cabecera, 100 + 2_000)
            },
            MUESTRAS_20_MS,
        ),
    ];
    for (p, muestras) in &casos {
        assert_eq!(decodificar(p).ok(), Some(*muestras), "{}", resumen(p));
    }
    // Multiflujo de dos flujos con una trama de 1275 bytes cada uno.
    let trama = con_carga(&[TOC_CELT_20_MS], MAX_BYTES_TRAMA);
    let paquete = common::build_multistream_packet(&[&trama, &trama]);
    let mut decodificador =
        OpusMultistreamDecoder::new(48_000, 2, 2, 0, &[0, 1]).expect("configuración válida");
    let mut pcm = vec![0i16; MAX_MUESTRAS_48K * 2];
    assert_eq!(
        decodificador.decode(&paquete, &mut pcm, false).ok(),
        Some(MUESTRAS_20_MS)
    );
}
/// El código 1 con la carga vacía son dos tramas de 0 bytes (`opus.c:269` solo exige una
/// longitud par), que se ocultan como las de la DTX; también como último subpaquete multiflujo.
#[test]
fn el_codigo_1_sin_carga_son_dos_tramas_vacias() {
    assert_eq!(
        decodificar(&[TOC_CELT_2_5_MS | CODIGO_1]).ok(),
        Some(2 * MUESTRAS_2_5_MS)
    );
    assert_eq!(
        decodificar(&[TOC_SILK_20_MS | CODIGO_1]).ok(),
        Some(2 * MUESTRAS_20_MS)
    );
    // Los dos flujos han de durar lo mismo: el primero, una trama de 5 ms.
    let primero = con_carga(&[TOC_CELT_5_MS], 2);
    let paquete = common::build_multistream_packet(&[&primero, &[TOC_CELT_2_5_MS | CODIGO_1]]);
    let mut decodificador =
        OpusMultistreamDecoder::new(48_000, 2, 2, 0, &[0, 1]).expect("configuración válida");
    let mut pcm = vec![0i16; MAX_MUESTRAS_48K * 2];
    assert_eq!(
        decodificador.decode(&paquete, &mut pcm, false).ok(),
        Some(2 * MUESTRAS_2_5_MS)
    );
}
/// `decode_float` con FEC valida el paquete antes de mirar la salida, como `decode` y libopus
/// (`opus_decode_native` analiza el paquete antes de comprobar `frame_size`).
#[test]
fn decode_float_con_fec_valida_el_paquete_antes_que_la_salida() {
    let bueno = con_carga(&[TOC_CELT_20_MS], 100);
    // Código 1 con una carga impar: incumple R3.
    let malo = [TOC_CELT_20_MS | CODIGO_1, 0];
    let mut entero = OpusDecoder::new(48_000, 2).expect("configuración válida");
    let mut flotante = OpusDecoder::new(48_000, 2).expect("configuración válida");
    let mut pcm = vec![0i16; MAX_MUESTRAS_48K * 2];
    let mut pcm_f = vec![0f32; MAX_MUESTRAS_48K * 2];
    entero
        .decode(&bueno, &mut pcm, false)
        .expect("paquete válido");
    flotante
        .decode_float(&bueno, &mut pcm_f, false)
        .expect("paquete válido");
    assert!(matches!(
        entero.decode(&malo, &mut pcm[..1], true),
        Err(OpusError::InvalidPacket)
    ));
    assert!(matches!(
        flotante.decode_float(&malo, &mut pcm_f[..1], true),
        Err(OpusError::InvalidPacket)
    ));
}

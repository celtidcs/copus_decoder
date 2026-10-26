//! Red de equivalencia: huella de la salida completa del decodificador sobre los 12 vectores
//! oficiales de RFC 8251, en varias frecuencias y números de canales, con pérdida de paquetes,
//! con paquetes dañados, en multiflujo y con transiciones de modo. Las huellas de `red/huellas.txt` salen de la versión
//! 0.1.1 original (más el parche, que no cambia la salida), salvo las que cambia a propósito una
//! corrección, listadas en `documentacion/historial-de-cambios.md`; toda refactorización debe
//! dejarlas idénticas.
//!
//! Ejecución: `OPUS_TESTVECTORS_DIR=<carpeta> cargo test --release --test red_equivalencia -- --ignored`.
//! Con `RED_ESCRIBIR=1` reescribe `red/huellas.txt` en vez de comparar (solo para fijar la base).

mod common;

use common::{build_multistream_packet, read_opus_demo_stream, vectors_dir};
use copus_decoder::{OpusDecoder, OpusMultistreamDecoder};
use std::fmt::Write as _;
use std::path::PathBuf;

/// Vectores oficiales de RFC 8251.
const NUMERO_DE_VECTORES: usize = 12;
/// Combinaciones de frecuencia de salida y canales que se decodifican de cada vector.
const SALIDAS: [(u32, usize); 6] = [
    (48_000, 2),
    (48_000, 1),
    (24_000, 2),
    (16_000, 1),
    (12_000, 2),
    (8_000, 1),
];
/// Uno de cada tantos paquetes se da por perdido en el caso de ocultación (PLC).
const PERIODO_DE_PERDIDA: usize = 9;
/// Uno de cada tantos paquetes se daña en el caso de paquetes dañados.
const PERIODO_DE_DANO: usize = 5;
/// Bytes cambiados en cada paquete dañado.
const BYTES_DANADOS: usize = 3;
/// Paquetes de cada bloque en el caso de transiciones de modo: un segundo de audio con tramas de
/// 20 ms, para que cada modo llegue a estabilizarse antes del siguiente cambio.
const BLOQUE_DE_TRANSICION: usize = 50;

/// Huella FNV-1a de 64 bits: detecta cualquier cambio de la salida (no es criptográfica ni lo
/// necesita: compara una versión del código con otra, no se defiende de nadie).
struct Huella(u64);

impl Huella {
    const BASE: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIMO: u64 = 0x0000_0100_0000_01b3;

    fn nueva() -> Self {
        Self(Self::BASE)
    }

    fn bytes(&mut self, datos: &[u8]) {
        for &b in datos {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(Self::PRIMO);
        }
    }

    fn resultado<E: std::fmt::Debug>(&mut self, resultado: &Result<usize, E>) {
        match resultado {
            Ok(n) => self.bytes(&(*n as u64).to_le_bytes()),
            Err(e) => self.bytes(format!("E:{e:?}").as_bytes()),
        }
    }
}

/// Daña un paquete de forma determinista sin tocar su primer byte (TOC).
fn danar(paquete: &[u8], semilla: u64) -> Vec<u8> {
    let mut salida = paquete.to_vec();
    if salida.len() < 2 {
        return salida;
    }
    let mut estado = semilla.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for _ in 0..BYTES_DANADOS {
        estado ^= estado >> 12;
        estado ^= estado << 25;
        estado ^= estado >> 27;
        let valor = estado.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let posicion = 1 + (valor as usize) % (salida.len() - 1);
        salida[posicion] ^= (valor >> 56) as u8 | 1;
    }
    salida
}

/// Modo de alimentar el decodificador con los paquetes de un vector.
#[derive(Clone, Copy)]
enum Modo {
    Normal,
    Perdidas,
    Danados,
}

/// Paquete que recibe el decodificador en la posición `n` según el modo (vacío = perdido).
fn entrada(modo: Modo, n: usize, paquete: &[u8]) -> Vec<u8> {
    match modo {
        Modo::Normal => paquete.to_vec(),
        Modo::Perdidas if n % PERIODO_DE_PERDIDA == PERIODO_DE_PERDIDA - 1 => Vec::new(),
        Modo::Perdidas => paquete.to_vec(),
        Modo::Danados if n % PERIODO_DE_DANO == PERIODO_DE_DANO - 1 => danar(paquete, n as u64),
        Modo::Danados => paquete.to_vec(),
    }
}

fn huella_float(paquetes: &[Vec<u8>], frecuencia: u32, canales: usize, modo: Modo) -> u64 {
    let mut decodificador = OpusDecoder::new(frecuencia, canales).unwrap();
    let mut pcm = vec![0f32; decodificador.max_frame_size_per_channel() * canales];
    let mut huella = Huella::nueva();
    for (n, paquete) in paquetes.iter().enumerate() {
        let resultado = decodificador.decode_float(&entrada(modo, n, paquete), &mut pcm, false);
        huella.resultado(&resultado);
        if let Ok(muestras) = resultado {
            for x in &pcm[..muestras * canales] {
                huella.bytes(&x.to_bits().to_le_bytes());
            }
        }
        huella.bytes(&decodificador.final_range().to_le_bytes());
    }
    huella.0
}

fn huella_i16(paquetes: &[Vec<u8>]) -> u64 {
    let mut decodificador = OpusDecoder::new(48_000, 2).unwrap();
    let mut pcm = vec![0i16; decodificador.max_frame_size_per_channel() * 2];
    let mut huella = Huella::nueva();
    for paquete in paquetes {
        let resultado = decodificador.decode(paquete, &mut pcm, false);
        huella.resultado(&resultado);
        if let Ok(muestras) = resultado {
            for x in &pcm[..muestras * 2] {
                huella.bytes(&x.to_le_bytes());
            }
        }
    }
    huella.0
}

/// Dos flujos mono de dos vectores distintos, en un multiflujo de dos canales.
fn huella_multiflujo(a: &[Vec<u8>], b: &[Vec<u8>]) -> u64 {
    let mut decodificador = OpusMultistreamDecoder::new(48_000, 2, 2, 0, &[0, 1]).unwrap();
    let mut pcm = vec![0f32; OpusDecoder::MAX_FRAME_SIZE_48K * 2];
    let mut huella = Huella::nueva();
    for (pa, pb) in a.iter().zip(b) {
        let resultado =
            decodificador.decode_float(&build_multistream_packet(&[pa, pb]), &mut pcm, false);
        huella.resultado(&resultado);
        if let Ok(muestras) = resultado {
            for x in &pcm[..muestras * 2] {
                huella.bytes(&x.to_bits().to_le_bytes());
            }
        }
    }
    huella.0
}

/// Bloques alternos de `BLOQUE_DE_TRANSICION` paquetes de `a` y de `b`, hasta agotar el más corto:
/// cada cambio de bloque es una transición de modo entre los dos vectores.
fn alternar(a: &[Vec<u8>], b: &[Vec<u8>]) -> Vec<Vec<u8>> {
    a.chunks(BLOQUE_DE_TRANSICION)
        .zip(b.chunks(BLOQUE_DE_TRANSICION))
        .flat_map(|(x, y)| x.iter().chain(y))
        .cloned()
        .collect()
}

/// Paquetes de un vector; los marcados como perdidos en el flujo oficial pasan vacíos.
fn paquetes(numero: usize) -> Vec<Vec<u8>> {
    let ruta = vectors_dir().join(format!("testvector{numero:02}.bit"));
    read_opus_demo_stream(&ruta)
        .unwrap_or_else(|e| panic!("no se puede leer {}: {e}", ruta.display()))
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect()
}

fn calcular_huellas() -> String {
    let mut lineas = String::new();
    let todos: Vec<_> = (1..=NUMERO_DE_VECTORES).map(paquetes).collect();
    for (i, p) in todos.iter().enumerate() {
        let v = i + 1;
        for (frecuencia, canales) in SALIDAS {
            let h = huella_float(p, frecuencia, canales, Modo::Normal);
            writeln!(
                lineas,
                "tv{v:02} f32 {frecuencia} {canales} normal {h:016x}"
            )
            .unwrap();
        }
        let h = huella_float(p, 48_000, 2, Modo::Perdidas);
        writeln!(lineas, "tv{v:02} f32 48000 2 perdidas {h:016x}").unwrap();
        let h = huella_float(p, 48_000, 2, Modo::Danados);
        writeln!(lineas, "tv{v:02} f32 48000 2 danados {h:016x}").unwrap();
        writeln!(lineas, "tv{v:02} i16 48000 2 normal {:016x}", huella_i16(p)).unwrap();
    }
    for (a, b) in [(1usize, 7usize), (5, 12)] {
        let h = huella_multiflujo(&todos[a - 1], &todos[b - 1]);
        writeln!(lineas, "multiflujo tv{a:02}+tv{b:02} {h:016x}").unwrap();
    }
    // Los vectores no pasan nunca de solo CELT a solo SILK: tv01 es solo CELT y tv02 solo SILK.
    let (a, b) = (1usize, 2usize);
    let h = huella_float(
        &alternar(&todos[a - 1], &todos[b - 1]),
        48_000,
        2,
        Modo::Normal,
    );
    writeln!(lineas, "transiciones tv{a:02}+tv{b:02} {h:016x}").unwrap();
    lineas
}

fn ruta_huellas() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("red/huellas.txt")
}

/// La salida coincide, huella a huella, con `red/huellas.txt`: las de la 0.1.1 original salvo las
/// que una corrección cambió a propósito (listadas en el historial de cambios).
#[test]
#[ignore = "necesita OPUS_TESTVECTORS_DIR con los vectores de RFC 8251 (ver documentacion/infraestructura)"]
fn la_salida_coincide_con_las_huellas() {
    let actuales = calcular_huellas();
    if std::env::var("RED_ESCRIBIR").as_deref() == Ok("1") {
        std::fs::write(ruta_huellas(), &actuales).unwrap();
        return;
    }
    let base = std::fs::read_to_string(ruta_huellas())
        .unwrap()
        .replace("\r\n", "\n");
    let distintas: Vec<_> = base
        .lines()
        .zip(actuales.lines())
        .filter(|(a, b)| a != b)
        .map(|(a, b)| format!("base {a}  ahora {b}"))
        .collect();
    assert_eq!(
        base.lines().count(),
        actuales.lines().count(),
        "cambia el número de casos"
    );
    assert!(
        distintas.is_empty(),
        "la salida cambió:\n{}",
        distintas.join("\n")
    );
}

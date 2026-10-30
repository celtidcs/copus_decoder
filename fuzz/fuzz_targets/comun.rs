//! Lectura de la entrada del fuzzer como una secuencia de llamadas al decodificador, compartida
//! por los objetivos.
//!
//! Formato: un byte que elige la configuración del decodificador y, después, llamadas. Cada
//! llamada es un byte de opciones, dos bytes con la longitud del paquete (little endian) y el
//! paquete, recortado a lo que quede de la entrada.

/// Frecuencias de salida que admite el decodificador.
pub const FRECUENCIAS: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// Llamadas como mucho por entrada, para que cada ejecución sea corta.
const MAX_LLAMADAS: usize = 64;

/// Opción de la llamada: decodificar una pérdida (paquete vacío) en vez del paquete.
const OPCION_PERDIDA: u8 = 1;
/// Opción de la llamada: pedir la FEC (`fec = true`).
const OPCION_FEC: u8 = 2;
/// Opción de la llamada: salida de solo una octava parte del tamaño máximo.
const OPCION_SALIDA_CORTA: u8 = 4;
/// Divisor del tamaño máximo de la salida con `OPCION_SALIDA_CORTA`: 720 muestras por canal, menos
/// que una trama de 20 ms a 48 kHz.
pub const DIVISOR_SALIDA_CORTA: usize = 8;

/// Una llamada al decodificador.
pub struct Llamada<'a> {
    /// El paquete; vacío para una pérdida.
    pub paquete: &'a [u8],
    /// Si se pide la FEC.
    pub fec: bool,
    /// Si la salida es corta (ver [`DIVISOR_SALIDA_CORTA`]).
    pub salida_corta: bool,
}

/// Cursor sobre la entrada del fuzzer.
pub struct Entrada<'a> {
    datos: &'a [u8],
    llamadas: usize,
}

impl<'a> Entrada<'a> {
    /// Empieza a leer `datos`.
    pub fn new(datos: &'a [u8]) -> Self {
        Self { datos, llamadas: 0 }
    }

    /// El siguiente byte, o `None` si se acabó la entrada.
    pub fn byte(&mut self) -> Option<u8> {
        let (&b, resto) = self.datos.split_first()?;
        self.datos = resto;
        Some(b)
    }

    /// La siguiente llamada, o `None` si se acabó la entrada o se llegó a [`MAX_LLAMADAS`].
    pub fn llamada(&mut self) -> Option<Llamada<'a>> {
        if self.llamadas == MAX_LLAMADAS {
            return None;
        }
        let opciones = self.byte()?;
        let longitud = usize::from(u16::from_le_bytes([self.byte()?, self.byte().unwrap_or(0)]));
        let (paquete, resto) = self.datos.split_at(longitud.min(self.datos.len()));
        self.datos = resto;
        self.llamadas += 1;
        Some(Llamada {
            paquete: if opciones & OPCION_PERDIDA != 0 {
                &[]
            } else {
                paquete
            },
            fec: opciones & OPCION_FEC != 0,
            salida_corta: opciones & OPCION_SALIDA_CORTA != 0,
        })
    }
}

/// Comprueba que `decode` y `decode_float` dan lo mismo: el mismo error, o las mismas muestras
/// (`n` por canal de `canales`) con la salida en coma flotante igual a la entera / 32768.
pub fn comprobar_iguales<E: std::fmt::Debug>(
    entero: Result<usize, E>,
    flotante: Result<usize, E>,
    pcm: &[i16],
    pcm_f: &[f32],
    canales: usize,
) {
    match (entero, flotante) {
        (Ok(n), Ok(m)) => {
            assert_eq!(n, m, "decode y decode_float dan duraciones distintas");
            assert!(n * canales <= pcm.len(), "más muestras que la salida");
            for (x, y) in pcm[..n * canales].iter().zip(&pcm_f[..n * canales]) {
                assert_eq!(
                    f32::from(*x) / 32_768.0,
                    *y,
                    "decode_float no es decode / 32768"
                );
            }
        }
        (Err(a), Err(b)) => assert_eq!(format!("{a:?}"), format!("{b:?}")),
        (a, b) => panic!("decode y decode_float discrepan: {a:?} frente a {b:?}"),
    }
}

//! Remuestreador de SILK del decodificador: del PCM a la frecuencia interna (8, 12 o 16 kHz) a
//! la de la API (8, 12, 16, 24 o 48 kHz), como `silk/resampler.c` de libopus.

use super::fixed::smulww;
use crate::Error;

use super::resampler_private::{self, DownFirShape};
use super::resampler_rom::{
    RESAMPLER_1_2_COEFS, RESAMPLER_2_3_COEFS, RESAMPLER_3_4_COEFS, RESAMPLER_DOWN_ORDER_FIR1,
};
use super::tables::{FS_KHZ_MB, FS_KHZ_NB, FS_KHZ_WB, HZ_PER_KHZ};

const RESAMPLER_MAX_BATCH_SIZE_MS: usize = 10;
pub(super) const SILK_RESAMPLER_MAX_IIR_ORDER: usize = 6;
const SILK_RESAMPLER_MAX_DELAY: usize = 96;
/// Mayor orden de los FIR de bajada del decodificador (el de 1/2), que fija la historia
/// guardada entre bloques.
const MAX_DOWN_FIR_ORDER: usize = RESAMPLER_DOWN_ORDER_FIR1;
/// Coeficientes del filtro AR de segundo orden al principio de cada tabla de bajada.
const AR2_COEFS: usize = 2;
/// Fases de los FIR de las bajadas 3/4 y 2/3.
const FRACS_3_4: usize = 3;
const FRACS_2_3: usize = 2;
/// Frecuencias de salida admitidas, en el orden de las columnas de [`DELAY_MATRIX_DEC`].
const API_RATES_HZ: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
/// Retardo de entrada, en muestras, de cada pareja de frecuencias: filas 8, 12 y 16 kHz;
/// columnas, las de [`API_RATES_HZ`] (`delay_matrix_dec` de `silk/resampler.c`, sin la columna
/// de 96 kHz).
const DELAY_MATRIX_DEC: [[usize; 5]; 3] = [[4, 0, 2, 0, 0], [0, 9, 4, 7, 4], [0, 3, 12, 7, 7]];

/// Método de remuestreo de una pareja de frecuencias, como `resampler_function` de
/// `silk_resampler_init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    /// Misma frecuencia: copia con el retardo de entrada.
    Copy,
    /// Salida al doble de la entrada: solo el duplicador de alta calidad.
    Up2Hq,
    /// Otras subidas: duplicador e interpolación FIR.
    IirFir,
    /// Bajadas: filtro AR de segundo orden e interpolación FIR, con los coeficientes `coefs`.
    DownFir {
        shape: DownFirShape,
        coefs: &'static [i16],
    },
}

/// Estado del remuestreador de SILK del decodificador.
#[derive(Debug, Clone)]
pub(super) struct SilkResampler {
    /// Frecuencia de entrada, en Hz.
    fs_in_hz: u32,
    /// Frecuencia de salida, en Hz.
    fs_out_hz: u32,
    /// Frecuencia de entrada, en kHz: muestras de entrada por milisegundo.
    fs_in_khz: usize,
    /// Frecuencia de salida, en kHz: muestras de salida por milisegundo.
    fs_out_khz: usize,
    /// Muestras de entrada de cada bloque que se procesa de una vez (10 ms).
    batch_size: usize,
    /// Paso del interpolador por muestra de salida, en Q16 (sobre la señal ya duplicada si el
    /// método es [`Method::IirFir`]).
    inv_ratio_q16: i32,
    /// Retardo de entrada, en muestras.
    input_delay: usize,
    /// Método de la pareja de frecuencias.
    method: Method,
    /// Estado de los filtros paso todo del duplicador o, en las bajadas, del filtro AR.
    s_iir: [i32; SILK_RESAMPLER_MAX_IIR_ORDER],
    /// Historia del interpolador FIR de las subidas.
    s_fir: [i16; resampler_private::fir_history_len()],
    /// Historia filtrada, en Q8, del FIR de las bajadas.
    s_down_fir: [i32; MAX_DOWN_FIR_ORDER],
    /// Memoria del retardo de entrada, que usa `process`.
    delay_buf: [i16; SILK_RESAMPLER_MAX_DELAY],
}

impl SilkResampler {
    /// Crea un remuestreador de la frecuencia interna `fs_in_hz` a la de la API `fs_out_hz`, sin
    /// estado previo, como `silk_resampler_init` con `forEnc == 0`.
    ///
    /// # Errores
    ///
    /// `NotImplemented` si la entrada no es de 8, 12 o 16 kHz o la salida no es de 8, 12, 16,
    /// 24 o 48 kHz.
    pub(super) fn new(fs_in_hz: u32, fs_out_hz: u32) -> Result<Self, Error> {
        let row = [FS_KHZ_NB, FS_KHZ_MB, FS_KHZ_WB]
            .iter()
            .position(|&khz| khz * HZ_PER_KHZ == fs_in_hz)
            .ok_or(Error::NotImplemented)?;
        let column = API_RATES_HZ
            .iter()
            .position(|&hz| hz == fs_out_hz)
            .ok_or(Error::NotImplemented)?;
        let method = Method::for_rates(fs_in_hz, fs_out_hz)?;
        let fs_in_khz = (fs_in_hz / HZ_PER_KHZ) as usize;
        // Las subidas con IIR y FIR interpolan sobre la señal ya duplicada.
        let up2x = u32::from(method == Method::IirFir);
        let mut inv_ratio_q16 =
            ((((fs_in_hz as i64) << (14 + up2x)) / fs_out_hz as i64) << 2) as i32;
        while smulww(inv_ratio_q16, fs_out_hz as i32) < ((fs_in_hz as i32) << up2x) {
            inv_ratio_q16 += 1;
        }

        Ok(Self {
            fs_in_hz,
            fs_out_hz,
            fs_in_khz,
            fs_out_khz: (fs_out_hz / HZ_PER_KHZ) as usize,
            batch_size: fs_in_khz * RESAMPLER_MAX_BATCH_SIZE_MS,
            inv_ratio_q16,
            input_delay: DELAY_MATRIX_DEC[row][column],
            method,
            s_iir: [0; SILK_RESAMPLER_MAX_IIR_ORDER],
            s_fir: [0; resampler_private::fir_history_len()],
            s_down_fir: [0; MAX_DOWN_FIR_ORDER],
            delay_buf: [0; SILK_RESAMPLER_MAX_DELAY],
        })
    }

    /// Si el remuestreador convierte de `fs_in_hz` a `fs_out_hz` y puede reutilizarse tal cual.
    pub(super) fn matches(&self, fs_in_hz: u32, fs_out_hz: u32) -> bool {
        self.fs_in_hz == fs_in_hz && self.fs_out_hz == fs_out_hz
    }

    /// Remuestrea `input`, a la frecuencia interna, en `output`, a la de la API, y devuelve las
    /// muestras escritas (`silk_resampler`). El primer milisegundo pasa por la memoria del
    /// retardo de entrada, que al final se rellena con las últimas muestras de `input`.
    pub(super) fn process(&mut self, input: &[i16], output: &mut [i16]) -> usize {
        debug_assert!(input.len() >= self.fs_in_khz);

        let n_samples = self.fs_in_khz - self.input_delay;
        self.delay_buf[self.input_delay..self.fs_in_khz].copy_from_slice(&input[..n_samples]);

        let first_block = self.delay_buf;
        self.run(&first_block[..self.fs_in_khz], output);
        let rest = &input[n_samples..n_samples + input.len() - self.fs_in_khz];
        self.run(rest, &mut output[self.fs_out_khz..]);

        let start = input.len() - self.input_delay;
        self.delay_buf[..self.input_delay].copy_from_slice(&input[start..]);
        self.output_len(input.len())
    }

    /// Remuestrea `input` en `output` con el método de la pareja de frecuencias.
    fn run(&mut self, input: &[i16], output: &mut [i16]) {
        match self.method {
            Method::Copy => output[..input.len()].copy_from_slice(input),
            Method::Up2Hq => {
                resampler_private::up2_hq(&mut self.s_iir, output, input, input.len());
            }
            Method::IirFir => self.process_iir_fir(input, output),
            Method::DownFir { shape, coefs } => self.process_down_fir(input, output, shape, coefs),
        }
    }

    /// Sobremuestrea `input` en `output`, en bloques de `batch_size` muestras: duplica la
    /// frecuencia con los filtros paso todo e interpola con el FIR
    /// (`silk_resampler_private_IIR_FIR`).
    fn process_iir_fir(&mut self, input: &[i16], output: &mut [i16]) {
        let history = resampler_private::fir_history_len();
        let mut buf = vec![0i16; 2 * self.batch_size + history];
        buf[..history].copy_from_slice(&self.s_fir);
        let mut written = 0usize;
        let mut in_offset = 0usize;
        let mut n_samples_in;
        loop {
            n_samples_in = (input.len() - in_offset).min(self.batch_size);
            resampler_private::up2_hq(
                &mut self.s_iir,
                &mut buf[history..history + 2 * n_samples_in],
                &input[in_offset..],
                n_samples_in,
            );
            let max_index_q16 = (n_samples_in as i32) << 17;
            written += resampler_private::iir_fir_interpol(
                &mut output[written..],
                &buf,
                max_index_q16,
                self.inv_ratio_q16,
            );
            in_offset += n_samples_in;
            if in_offset >= input.len() {
                break;
            }
            buf.copy_within(2 * n_samples_in..2 * n_samples_in + history, 0);
        }
        self.s_fir
            .copy_from_slice(&buf[2 * n_samples_in..2 * n_samples_in + history]);
    }

    /// Submuestrea `input` en `output`, en bloques de `batch_size` muestras: filtra con el AR de
    /// segundo orden de `coefs` e interpola con su FIR (`silk_resampler_private_down_FIR`).
    fn process_down_fir(
        &mut self,
        input: &[i16],
        output: &mut [i16],
        shape: DownFirShape,
        coefs: &'static [i16],
    ) {
        let order = shape.order();
        let mut buf = vec![0i32; self.batch_size + order];
        buf[..order].copy_from_slice(&self.s_down_fir[..order]);
        let (ar_coefs, fir_coefs) = coefs.split_at(AR2_COEFS);
        let mut written = 0usize;
        let mut in_offset = 0usize;
        let mut n_samples_in;
        loop {
            n_samples_in = (input.len() - in_offset).min(self.batch_size);
            resampler_private::ar2(
                &mut self.s_iir,
                &mut buf[order..order + n_samples_in],
                &input[in_offset..in_offset + n_samples_in],
                ar_coefs,
            );
            let max_index_q16 = (n_samples_in as i32) << 16;
            written += resampler_private::down_fir_interpol(
                &mut output[written..],
                &buf,
                fir_coefs,
                shape,
                max_index_q16,
                self.inv_ratio_q16,
            );
            in_offset += n_samples_in;
            // Como en libopus, el bucle sigue mientras quede más de una muestra.
            if input.len() - in_offset <= 1 {
                break;
            }
            buf.copy_within(n_samples_in..n_samples_in + order, 0);
        }
        self.s_down_fir[..order].copy_from_slice(&buf[n_samples_in..n_samples_in + order]);
    }

    /// Muestras de salida que dan `input_len` muestras de entrada con esta pareja de frecuencias.
    pub(super) fn output_len(&self, input_len: usize) -> usize {
        ((input_len as u64) * self.fs_out_hz as u64 / self.fs_in_hz as u64) as usize
    }
}

impl Method {
    /// El método de `silk_resampler_init` para la pareja de frecuencias.
    ///
    /// # Errores
    ///
    /// `NotImplemented` si es una bajada sin FIR en el decodificador (ninguna de las parejas
    /// admitidas por [`SilkResampler::new`]).
    fn for_rates(fs_in_hz: u32, fs_out_hz: u32) -> Result<Self, Error> {
        use std::cmp::Ordering;
        Ok(match fs_out_hz.cmp(&fs_in_hz) {
            Ordering::Equal => Self::Copy,
            Ordering::Greater if fs_out_hz == 2 * fs_in_hz => Self::Up2Hq,
            Ordering::Greater => Self::IirFir,
            Ordering::Less if 4 * fs_out_hz == 3 * fs_in_hz => Self::DownFir {
                shape: DownFirShape::Phased { fracs: FRACS_3_4 },
                coefs: &RESAMPLER_3_4_COEFS,
            },
            Ordering::Less if 3 * fs_out_hz == 2 * fs_in_hz => Self::DownFir {
                shape: DownFirShape::Phased { fracs: FRACS_2_3 },
                coefs: &RESAMPLER_2_3_COEFS,
            },
            Ordering::Less if 2 * fs_out_hz == fs_in_hz => Self::DownFir {
                shape: DownFirShape::Symmetric,
                coefs: &RESAMPLER_1_2_COEFS,
            },
            Ordering::Less => return Err(Error::NotImplemented),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::SilkResampler;

    /// Referencia de `silk_resampler` de libopus 1.6.1 en las bajadas del decodificador (16 a 12,
    /// 16 a 8 y 12 a 8 kHz), generada con `documentacion/infraestructura/referencia-libopus/resto1.c`:
    /// por pareja (`pareja <entrada> <salida>`), seis llamadas sobre el mismo estado
    /// (`llamada <n> entrada <n muestras>` y `salida <escritas> <muestras>`), con longitudes que
    /// dejan una muestra suelta al final del bucle por bloques de `down_FIR` y otras que no.
    const REFERENCIA: &str = include_str!("../../tests/datos/remuestreo_resto1.txt");
    const PAREJAS: usize = 3;
    const LLAMADAS_POR_PAREJA: usize = 6;
    /// Sitio de sobra para la salida de cualquier llamada de la referencia.
    const MAX_SALIDA: usize = 4096;
    /// Centinelas distintos con que se rellena la salida antes de cada llamada (los del arnés).
    const CENTINELA_A: i16 = 0x7ABC;
    const CENTINELA_B: i16 = -0x1234;

    /// Números de una línea de la referencia tras sus `prefijo` palabras.
    fn numeros(linea: &str, prefijo: usize) -> Vec<i32> {
        linea
            .split_whitespace()
            .skip(prefijo)
            .map(|v| v.parse().unwrap())
            .collect()
    }

    /// Como libopus, `down_FIR` sale del bucle por bloques en cuanto queda una muestra o ninguna
    /// (`inLen > 1`); la muestra suelta se procesa en la llamada siguiente. Cada llamada escribe
    /// las mismas muestras que libopus, con el estado heredado de las anteriores.
    #[test]
    fn down_fir_matches_libopus_with_one_sample_left() {
        let mut lineas = REFERENCIA.lines();
        let mut parejas = 0;
        let mut llamadas = 0;
        while let Some(cabecera) = lineas.next() {
            let pareja = numeros(cabecera, 1);
            assert!(
                cabecera.starts_with("pareja ") && pareja.len() == 2,
                "{cabecera}"
            );
            let mut resampler = SilkResampler::new(pareja[0] as u32, pareja[1] as u32).unwrap();
            for _ in 0..LLAMADAS_POR_PAREJA {
                let entrada = lineas.next().unwrap();
                let salida = lineas.next().unwrap();
                assert!(entrada.starts_with("llamada ") && salida.starts_with("salida "));
                let n: usize = entrada.split_whitespace().nth(1).unwrap().parse().unwrap();
                let input: Vec<i16> = numeros(entrada, 3).iter().map(|&v| v as i16).collect();
                assert_eq!(input.len(), n);
                let esperada = numeros(salida, 1);
                let escritas = esperada[0] as usize;
                // Como el arnés de libopus: las muestras escritas son las que no dependen del
                // centinela con que se rellena la salida.
                let mut copia = resampler.clone();
                let mut con_a = vec![CENTINELA_A; MAX_SALIDA];
                let mut con_b = vec![CENTINELA_B; MAX_SALIDA];
                copia.process(&input, &mut con_a);
                let n_out = resampler.process(&input, &mut con_b);
                let escritas_nuestras = con_a
                    .iter()
                    .zip(&con_b)
                    .rposition(|(a, b)| a == b)
                    .map_or(0, |i| i + 1);
                let llamada = format!(
                    "{}->{} Hz, llamada {llamadas} ({n} muestras)",
                    pareja[0], pareja[1]
                );
                assert_eq!(escritas_nuestras, escritas, "{llamada}: muestras escritas");
                let obtenida: Vec<i32> = con_a[..escritas].iter().map(|&v| i32::from(v)).collect();
                assert_eq!(obtenida, esperada[1..], "{llamada}: salida");
                // Lo que devuelve es la cuenta del llamador de libopus (`silk_Decode`), no las
                // escritas: con un resto de una muestra, una menos.
                assert_eq!(
                    n_out,
                    n * pareja[1] as usize / pareja[0] as usize,
                    "{llamada}"
                );
                llamadas += 1;
            }
            parejas += 1;
        }
        assert_eq!(parejas, PAREJAS);
        assert_eq!(llamadas, PAREJAS * LLAMADAS_POR_PAREJA);
    }
}

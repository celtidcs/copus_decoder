//! Remuestreador de SILK del decodificador: del PCM a la frecuencia interna a 48 kHz.

use super::fixed::smulww;
use crate::Error;

use super::resampler_private;
use super::tables::{FS_KHZ_MB, FS_KHZ_NB, FS_KHZ_WB, HZ_PER_KHZ};

const RESAMPLER_MAX_BATCH_SIZE_MS: usize = 10;
pub(super) const SILK_RESAMPLER_MAX_IIR_ORDER: usize = 6;
const SILK_RESAMPLER_MAX_DELAY: usize = 96;
/// Frecuencia de salida del único camino del decodificador: la de la API, 48 kHz.
const FS_OUT_48K_HZ: u32 = crate::packet::OPUS_RATE_HZ as u32;
/// Retardo de entrada hacia 48 kHz desde 8, 12 y 16 kHz: columna de 48 kHz de
/// `delay_matrix_dec` en `silk_resampler_init`.
const INPUT_DELAY_TO_48K: [usize; 3] = [0, 4, 7];

/// Estado del remuestreador de SILK del decodificador.
#[derive(Debug, Clone)]
pub(super) struct SilkResampler {
    /// Frecuencia de entrada, en Hz.
    fs_in_hz: u32,
    /// Frecuencia de salida, en Hz.
    fs_out_hz: u32,
    /// Frecuencia de entrada, en kHz: muestras de entrada por milisegundo.
    fs_in_khz: usize,
    /// Muestras de entrada de cada bloque que se procesa de una vez (10 ms).
    batch_size: usize,
    /// Paso del interpolador FIR por muestra de salida, en Q16, sobre la señal ya duplicada.
    inv_ratio_q16: i32,
    /// Retardo de entrada del decodificador, en muestras.
    input_delay: usize,
    /// Estado de los filtros paso todo del duplicador.
    s_iir: [i32; SILK_RESAMPLER_MAX_IIR_ORDER],
    /// Historia del interpolador FIR.
    s_fir: [i16; resampler_private::fir_history_len()],
    /// Memoria del retardo de entrada, que usa `process`.
    delay_buf: [i16; SILK_RESAMPLER_MAX_DELAY],
}

impl SilkResampler {
    /// Crea un remuestreador de la frecuencia interna `fs_in_hz` a la de la API `fs_out_hz`, sin
    /// estado previo.
    ///
    /// # Errores
    ///
    /// `NotImplemented` si la entrada no es de 8, 12 o 16 kHz o la salida no es de 48 kHz.
    pub(super) fn new(fs_in_hz: u32, fs_out_hz: u32) -> Result<Self, Error> {
        const FS_NB_HZ: u32 = FS_KHZ_NB * HZ_PER_KHZ;
        const FS_MB_HZ: u32 = FS_KHZ_MB * HZ_PER_KHZ;
        const FS_WB_HZ: u32 = FS_KHZ_WB * HZ_PER_KHZ;
        let input_delay = match (fs_in_hz, fs_out_hz) {
            (FS_NB_HZ, FS_OUT_48K_HZ) => INPUT_DELAY_TO_48K[0],
            (FS_MB_HZ, FS_OUT_48K_HZ) => INPUT_DELAY_TO_48K[1],
            (FS_WB_HZ, FS_OUT_48K_HZ) => INPUT_DELAY_TO_48K[2],
            _ => return Err(Error::NotImplemented),
        };
        let fs_in_khz = (fs_in_hz / HZ_PER_KHZ) as usize;
        let batch_size = fs_in_khz * RESAMPLER_MAX_BATCH_SIZE_MS;
        let mut inv_ratio_q16 = ((((fs_in_hz as i64) << 15) / fs_out_hz as i64) << 2) as i32;
        while smulww(inv_ratio_q16, fs_out_hz as i32) < ((fs_in_hz as i32) << 1) {
            inv_ratio_q16 += 1;
        }

        Ok(Self {
            fs_in_hz,
            fs_out_hz,
            fs_in_khz,
            batch_size,
            inv_ratio_q16,
            input_delay,
            s_iir: [0; SILK_RESAMPLER_MAX_IIR_ORDER],
            s_fir: [0; resampler_private::fir_history_len()],
            delay_buf: [0; SILK_RESAMPLER_MAX_DELAY],
        })
    }

    /// Si el remuestreador convierte de `fs_in_hz` a `fs_out_hz` y puede reutilizarse tal cual.
    pub(super) fn matches(&self, fs_in_hz: u32, fs_out_hz: u32) -> bool {
        self.fs_in_hz == fs_in_hz && self.fs_out_hz == fs_out_hz
    }

    /// Remuestrea `input`, a la frecuencia interna, en `output`, a la de la API, y devuelve las
    /// muestras escritas. El primer milisegundo pasa por la memoria del retardo de entrada, que
    /// al final se rellena con las últimas muestras de `input`.
    pub(super) fn process(&mut self, input: &[i16], output: &mut [i16]) -> usize {
        debug_assert!(input.len() >= self.fs_in_khz);

        let n_samples = self.fs_in_khz - self.input_delay;
        self.delay_buf[self.input_delay..self.fs_in_khz].copy_from_slice(&input[..n_samples]);

        let mut written = 0usize;
        let first_block = self.delay_buf[..self.fs_in_khz].to_vec();
        written += self.process_iir_fir(&first_block, &mut output[written..]);
        let second_len = input.len().saturating_sub(self.fs_in_khz);
        written += self.process_iir_fir(
            &input[n_samples..n_samples + second_len],
            &mut output[written..],
        );

        if self.input_delay > 0 {
            let start = input.len() - self.input_delay;
            self.delay_buf[..self.input_delay].copy_from_slice(&input[start..]);
        }
        written
    }

    /// Sobremuestrea `input` en `output`, en bloques de `batch_size` muestras: duplica la
    /// frecuencia con los filtros paso todo e interpola con el FIR. Devuelve las muestras
    /// escritas.
    fn process_iir_fir(&mut self, input: &[i16], output: &mut [i16]) -> usize {
        if input.is_empty() {
            return 0;
        }

        let history = resampler_private::fir_history_len();
        let mut total_written = 0usize;
        let mut in_offset = 0usize;

        while in_offset < input.len() {
            let n_samples_in = (input.len() - in_offset).min(self.batch_size);
            let mut buf = vec![0i16; history + (2 * n_samples_in) + history];
            buf[..history].copy_from_slice(&self.s_fir);
            resampler_private::up2_hq(
                &mut self.s_iir,
                &mut buf[history..history + 2 * n_samples_in],
                &input[in_offset..in_offset + n_samples_in],
                n_samples_in,
            );

            let max_index_q16 = (n_samples_in as i32) << 17;
            let written = resampler_private::iir_fir_interpol(
                &mut output[total_written..],
                &buf,
                max_index_q16,
                self.inv_ratio_q16,
            );
            total_written += written;

            let src_start = 2 * n_samples_in;
            self.s_fir
                .copy_from_slice(&buf[src_start..src_start + history]);
            in_offset += n_samples_in;
        }

        total_written
    }

    /// Muestras de salida que dan `input_len` muestras de entrada con esta pareja de frecuencias.
    pub(super) fn output_len(&self, input_len: usize) -> usize {
        ((input_len as u64) * self.fs_out_hz as u64 / self.fs_in_hz as u64) as usize
    }
}

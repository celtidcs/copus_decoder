//! Síntesis CELT de un canal: MDCT inversa del espectro desnormalizado sobre la memoria de
//! síntesis (`compute_inv_mdcts` de `celt/celt_decoder.c` en libopus).

use super::modes::CeltMode;
use super::{CeltDecoder, DECODE_BUFFER_SIZE, MAX_LM, lm_for_frame_size, mdct};
use crate::Error;

/// Forma temporal de la trama que se sintetiza.
#[derive(Debug, Clone, Copy)]
pub(super) struct SynthesisShape {
    /// Muestras por canal a 48 kHz (120, 240, 480 o 960).
    pub frame_size_48k: usize,
    /// Logaritmo en base 2 del número de bloques cortos de la trama.
    pub lm: usize,
    /// Trama transitoria: `1 << lm` bloques cortos entrelazados en vez de un bloque largo.
    pub is_transient: bool,
}

/// Una MDCT inversa por cada duración de trama, con la ventana y el solape del modo.
///
/// Invariante: `by_lm[lm]` es la MDCT de `2 · 120 << lm` muestras (`shift` = `MAX_LM - lm`), así que `by_lm[0]` es la del
/// bloque corto de 120 muestras.
#[derive(Debug, Clone)]
pub(super) struct MdctBank {
    by_lm: [mdct::MdctBackward; MAX_LM + 1],
    window: &'static [f32],
    overlap: usize,
}

impl MdctBank {
    /// Prepara las cuatro MDCT inversas del modo.
    pub(super) fn new(mode: &'static CeltMode) -> Self {
        Self {
            by_lm: std::array::from_fn(|lm| mdct::MdctBackward::new(MAX_LM - lm, mode.overlap)),
            window: mode.window,
            overlap: mode.overlap,
        }
    }

    /// Desplaza la memoria de síntesis `mem` una trama hacia atrás y escribe al final la MDCT
    /// inversa de `spectrum`, solapada con la cola de la trama anterior.
    ///
    /// Errores: `Error::NotImplemented` si `shape.frame_size_48k` no es una duración CELT o si la
    /// MDCT rechaza las longitudes.
    pub(super) fn synthesise_into(
        &self,
        spectrum: &[f32],
        shape: SynthesisShape,
        mem: &mut [f32],
    ) -> Result<(), Error> {
        let SynthesisShape {
            frame_size_48k,
            lm,
            is_transient,
        } = shape;
        let decode_len = DECODE_BUFFER_SIZE + self.overlap;
        mem.copy_within(frame_size_48k..decode_len, 0);
        let out_start = CeltDecoder::out_start(frame_size_48k);
        if is_transient {
            let m_blocks = 1usize << lm;
            let short_len = frame_size_48k / m_blocks;
            let mut short_coeffs = vec![0.0f32; short_len];
            for b in 0..m_blocks {
                let out_offset = out_start + b * short_len;
                for (j, coeff) in short_coeffs.iter_mut().enumerate() {
                    *coeff = spectrum[j * m_blocks + b];
                }
                self.by_lm[0]
                    .backward(
                        &short_coeffs,
                        self.window,
                        &mut mem[out_offset..out_offset + short_len + self.overlap],
                    )
                    .map_err(|_| Error::NotImplemented)?;
            }
        } else {
            let mdct = lm_for_frame_size(frame_size_48k)
                .map(|frame_lm| &self.by_lm[frame_lm])
                .ok_or(Error::NotImplemented)?;
            mdct.backward(
                spectrum,
                self.window,
                &mut mem[out_start..out_start + frame_size_48k + self.overlap],
            )
            .map_err(|_| Error::NotImplemented)?;
        }
        Ok(())
    }
}

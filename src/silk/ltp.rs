//! Predicción a largo plazo (LTP) de SILK.

use super::entropy_tables::{LTP_GAIN_ICDF_0, LTP_GAIN_ICDF_1, LTP_GAIN_ICDF_2};
use super::fixed::smlawb;
use super::tables::{
    LTP_ORDER, LTP_SCALES_Q14, LTP_VQ_0_Q7, LTP_VQ_1_Q7, LTP_VQ_2_Q7, MAX_NB_SUBFR,
};

/// Clase de periodicidad LTP (`PERIndex`, RFC 6716 §4.2.7.6.1).
///
/// Elige a la vez la tabla de ganancias con que se decodifican los índices LTP y el libro de
/// filtros de 8, 16 o 32 entradas del que salen. Solo hay tres clases y el tipo no admite otras,
/// de modo que elegir el libro no puede fallar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum LtpPeriodicity {
    /// Índice 0: libro de 8 filtros.
    #[default]
    Codebook8,
    /// Índice 1: libro de 16 filtros.
    Codebook16,
    /// Índice 2: libro de 32 filtros.
    Codebook32,
}

impl LtpPeriodicity {
    /// Clase del índice decodificado del flujo, o `None` si no es 0, 1 ni 2.
    pub(super) fn from_index(index: i32) -> Option<Self> {
        match index {
            0 => Some(Self::Codebook8),
            1 => Some(Self::Codebook16),
            2 => Some(Self::Codebook32),
            _ => None,
        }
    }

    /// iCDF con que se decodifican los índices de filtro LTP de esta clase.
    pub(super) fn gain_icdf(self) -> &'static [u8] {
        match self {
            Self::Codebook8 => &LTP_GAIN_ICDF_0,
            Self::Codebook16 => &LTP_GAIN_ICDF_1,
            Self::Codebook32 => &LTP_GAIN_ICDF_2,
        }
    }

    /// Libro de filtros LTP de esta clase, en Q7.
    fn vq_codebook(self) -> &'static [[i8; LTP_ORDER]] {
        match self {
            Self::Codebook8 => &LTP_VQ_0_Q7,
            Self::Codebook16 => &LTP_VQ_1_Q7,
            Self::Codebook32 => &LTP_VQ_2_Q7,
        }
    }
}

/// Coeficientes LTP en Q14 de las `nb_subfr` subtramas, sacados con `ltp_indices` del libro que
/// elige `periodicity`, y la escala LTP en Q14 de `ltp_scale_index`.
pub(super) fn decode_ltp_coeffs(
    periodicity: LtpPeriodicity,
    ltp_indices: &[i8; MAX_NB_SUBFR],
    nb_subfr: usize,
    ltp_scale_index: i8,
) -> ([[i16; LTP_ORDER]; MAX_NB_SUBFR], i32) {
    let mut coeffs_q14 = [[0i16; LTP_ORDER]; MAX_NB_SUBFR];
    let ltp_scale_q14 = LTP_SCALES_Q14[ltp_scale_index as usize] as i32;
    let codebook = periodicity.vq_codebook();

    for k in 0..nb_subfr {
        let row = &codebook[ltp_indices[k] as usize];
        for i in 0..LTP_ORDER {
            coeffs_q14[k][i] = (row[i] as i16) << 7;
        }
    }

    (coeffs_q14, ltp_scale_q14)
}

/// Aplica a una subtrama de `subfr_length` muestras el filtro LTP de 5 coeficientes
/// `ltp_coefs_q14`: escribe en `pres_q14` la excitación `exc_q14` más la predicción a partir de
/// la historia `ltp_q15` (en Q15) retrasada `pitch_lag`. Añade cada residuo a la historia desde
/// `buf_idx`, que avanza.
pub(super) fn ltp_filter(
    pres_q14: &mut [i32],
    exc_q14: &[i32],
    ltp_q15: &mut [i32],
    buf_idx: &mut usize,
    pitch_lag: usize,
    subfr_length: usize,
    ltp_coefs_q14: &[i16; LTP_ORDER],
) {
    let start = *buf_idx - pitch_lag + (LTP_ORDER / 2);
    for i in 0..subfr_length {
        let pred_base = start + i;
        let mut ltp_pred_q13 = 2i32;
        ltp_pred_q13 = smlawb(ltp_pred_q13, ltp_q15[pred_base], ltp_coefs_q14[0] as i32);
        ltp_pred_q13 = smlawb(
            ltp_pred_q13,
            ltp_q15[pred_base - 1],
            ltp_coefs_q14[1] as i32,
        );
        ltp_pred_q13 = smlawb(
            ltp_pred_q13,
            ltp_q15[pred_base - 2],
            ltp_coefs_q14[2] as i32,
        );
        ltp_pred_q13 = smlawb(
            ltp_pred_q13,
            ltp_q15[pred_base - 3],
            ltp_coefs_q14[3] as i32,
        );
        ltp_pred_q13 = smlawb(
            ltp_pred_q13,
            ltp_q15[pred_base - 4],
            ltp_coefs_q14[4] as i32,
        );
        pres_q14[i] = exc_q14[i].saturating_add(ltp_pred_q13 << 1);
        ltp_q15[*buf_idx] = pres_q14[i] << 1;
        *buf_idx += 1;
    }
}

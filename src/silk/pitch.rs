//! Decodificación de los lags de tono de SILK.

use super::tables::{
    CB_LAGS_STAGE2_10MS_NB, CB_LAGS_STAGE2_NB, CB_LAGS_STAGE3_10MS_WB, CB_LAGS_STAGE3_WB,
    MAX_NB_SUBFR, PITCH_EST_MAX_LAG_MS, PITCH_EST_MIN_LAG_MS,
};

/// Lag de tono, en muestras, de cada una de las `nb_subfr` subtramas: el lag absoluto de
/// `lag_index` más el desplazamiento del contorno `contour_index` a la frecuencia interna
/// `fs_khz`, acotado al rango de lags admitido.
pub(super) fn decode_pitch_lags(
    lag_index: i32,
    contour_index: i32,
    fs_khz: i32,
    nb_subfr: usize,
) -> [i32; MAX_NB_SUBFR] {
    let min_lag = PITCH_EST_MIN_LAG_MS * fs_khz;
    let max_lag = PITCH_EST_MAX_LAG_MS * fs_khz;
    let lag = min_lag + lag_index;
    let mut pitch_lags = [0i32; MAX_NB_SUBFR];

    for (k, pitch_lag) in pitch_lags.iter_mut().enumerate().take(nb_subfr) {
        let contour = contour_value(fs_khz, nb_subfr, k, contour_index as usize) as i32;
        *pitch_lag = (lag + contour).clamp(min_lag, max_lag);
    }

    pitch_lags
}

/// Desplazamiento en muestras del contorno de tono `contour_index` en la subtrama `subframe`,
/// según la frecuencia interna `fs_khz` y las `nb_subfr` subtramas.
///
/// Una trama SILK tiene 4 subtramas (20 ms) o 2 (10 ms); `internal_frame_shape` rechaza
/// cualquier otra duración antes de llegar aquí. Por eso basta distinguir «4 o no».
fn contour_value(fs_khz: i32, nb_subfr: usize, subframe: usize, contour_index: usize) -> i8 {
    let narrowband = fs_khz == super::tables::FS_KHZ_NB as i32;
    let twenty_ms = nb_subfr == MAX_NB_SUBFR;
    match (narrowband, twenty_ms) {
        (true, true) => CB_LAGS_STAGE2_NB[subframe][contour_index],
        (true, false) => CB_LAGS_STAGE2_10MS_NB[subframe][contour_index],
        (false, true) => CB_LAGS_STAGE3_WB[subframe][contour_index],
        (false, false) => CB_LAGS_STAGE3_10MS_WB[subframe][contour_index],
    }
}

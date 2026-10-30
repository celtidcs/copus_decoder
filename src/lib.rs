//! Decodificador Opus (RFC 6716, con las correcciones de RFC 8251) en Rust puro, sin código
//! `unsafe`.
//!
//! La API pública convierte paquetes en PCM: [`OpusDecoder`] para un flujo y
//! [`OpusMultistreamDecoder`] para varios (RFC 7845). Por dentro, CELT y SILK siguen de cerca la
//! estructura de libopus.

#![forbid(unsafe_code)]

mod celt;
mod decoder;
mod entropy;
mod error;
mod multistream;
mod packet;
mod pcm;
mod silk;
#[cfg(test)]
mod test_vectors;

/// Escala de una muestra `i16` en coma flotante: `i16::MIN` pasa a -1,0.
const I16_FULL_SCALE: f32 = 32_768.0;
pub(crate) use decoder::Decoder;
pub(crate) use error::Error;
pub use multistream::OpusMultistreamDecoder;

/// Lo que se pide al decodificador interno en cada llamada a `decode` o `decode_float`.
#[derive(Debug, Clone, Copy)]
enum Request<'a> {
    /// Decodificar el paquete.
    Packet(&'a [u8]),
    /// Ocultar una pérdida.
    Loss,
    /// Recuperar la pérdida anterior con la corrección de errores de la banda del paquete.
    Fec(&'a [u8]),
}

/// Decodificador Opus de un flujo.
///
/// Convierte cada paquete en PCM entrelazado y guarda entre paquetes el estado de CELT y de
/// SILK, que no se expone.
pub struct OpusDecoder {
    decoder: Decoder,
    /// Búfer `i16` de [`OpusDecoder::decode_float`], que se reutiliza entre llamadas.
    float_scratch: Vec<i16>,
}

impl OpusDecoder {
    /// Máximo de muestras por canal de un paquete a 48 kHz (120 ms).
    pub const MAX_FRAME_SIZE_48K: usize = Decoder::MAX_FRAME_SIZE_48K;

    /// Crea un decodificador con frecuencia de salida `sample_rate` y `channels` canales.
    ///
    /// # Errores
    ///
    /// `InvalidArgument("sample_rate")` si la frecuencia no es 8000, 12000, 16000, 24000 ni
    /// 48000, e `InvalidArgument("channels")` si los canales no son 1 ni 2.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use copus_decoder::OpusDecoder;
    ///
    /// let decoder = OpusDecoder::new(48_000, 2)?;
    /// # let _ = decoder;
    /// # Ok::<(), copus_decoder::OpusError>(())
    /// ```
    pub fn new(sample_rate: u32, channels: usize) -> Result<Self, OpusError> {
        let channels =
            u8::try_from(channels).map_err(|_| OpusError::InvalidArgument("channels"))?;
        let decoder = Decoder::new(sample_rate, channels).map_err(OpusError::from)?;

        Ok(Self {
            decoder,
            float_scratch: Vec::new(),
        })
    }

    /// Máximo de muestras por canal de un paquete a la frecuencia de salida (120 ms).
    pub fn max_frame_size_per_channel(&self) -> usize {
        self.decoder.max_frame_size_per_channel()
    }

    /// Muestras por canal del último paquete decodificado sin error (0 tras crear o reiniciar),
    /// que es lo que dura la ocultación de una pérdida.
    pub(crate) fn last_packet_duration(&self) -> usize {
        self.decoder.last_packet_duration()
    }

    /// Lo que se pide con `packet` y `fec`: con un paquete vacío, ocultar una pérdida; con `fec`,
    /// recuperar la pérdida anterior con la corrección de errores de la banda de `packet`; si no,
    /// decodificar `packet`.
    fn request(packet: &[u8], fec: bool) -> Request<'_> {
        if packet.is_empty() {
            Request::Loss
        } else if fec {
            Request::Fec(packet)
        } else {
            Request::Packet(packet)
        }
    }

    /// Atiende `request` con el decodificador interno, escribiendo PCM `i16` en `pcm`.
    fn run(&mut self, request: Request<'_>, pcm: &mut [i16]) -> Result<usize, Error> {
        match request {
            Request::Packet(packet) => self.decoder.decode(Some(packet), pcm),
            Request::Loss => self.decoder.decode(None, pcm),
            Request::Fec(packet) => self.decoder.decode_fec(packet, pcm),
        }
    }

    /// Decodifica un paquete en PCM `i16`, entrelazado si hay dos canales, y devuelve las
    /// muestras por canal.
    ///
    /// Un `packet` vacío oculta una pérdida a partir del estado anterior. Con `fec` a `true`,
    /// `packet` es el paquete que sigue a una pérdida y se usa su corrección de errores de la
    /// banda (FEC: tramas LBRR de SILK) para recuperar la pérdida, como `decode_fec` en libopus:
    /// la salida dura lo que el último paquete decodificado, y si `packet` no trae con qué
    /// recuperarla, se oculta. Después hay que decodificar `packet` otra vez con `fec` a `false`.
    /// [`packet_has_lbrr`] dice si `packet` trae FEC. En la decodificación normal se pasa `false`.
    ///
    /// # Errores
    ///
    /// `InvalidPacket` si el paquete no es válido, `BufferTooSmall` si la trama no cabe en `pcm`
    /// e `InternalError` si se llega a un estado interno no previsto.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use copus_decoder::OpusDecoder;
    ///
    /// let mut decoder = OpusDecoder::new(48_000, 2)?;
    /// let packet = std::fs::read("frame.opus")?;
    /// let mut pcm = vec![0i16; 960 * 2];
    /// let samples = decoder.decode(&packet, &mut pcm, false)?;
    /// # let _ = samples;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn decode(
        &mut self,
        packet: &[u8],
        pcm: &mut [i16],
        fec: bool,
    ) -> Result<usize, OpusError> {
        self.run(Self::request(packet, fec), pcm)
            .map_err(OpusError::from)
    }

    /// Decodifica un paquete en PCM `f32` en `[-1, 1)`, entrelazado si hay dos canales; por lo
    /// demás, como [`OpusDecoder::decode`].
    ///
    /// En esta versión, la salida es la de [`OpusDecoder::decode`] dividida entre 32 768: tiene la
    /// resolución de 16 bits, no la de la salida en coma flotante de libopus.
    ///
    /// # Errores
    ///
    /// Los de [`OpusDecoder::decode`].
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use copus_decoder::OpusDecoder;
    ///
    /// let mut decoder = OpusDecoder::new(48_000, 1)?;
    /// let packet = std::fs::read("frame.opus")?;
    /// let mut pcm = vec![0.0f32; 960];
    /// let samples = decoder.decode_float(&packet, &mut pcm, false)?;
    /// # let _ = samples;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn decode_float(
        &mut self,
        packet: &[u8],
        pcm: &mut [f32],
        fec: bool,
    ) -> Result<usize, OpusError> {
        let request = Self::request(packet, fec);
        // Como `decode` y libopus, el paquete se valida antes de mirar la salida, también el de
        // la FEC, aunque la salida dure lo que el último paquete.
        let samples_per_channel_hint = match request {
            Request::Loss => self.decoder.last_packet_duration(),
            Request::Fec(packet) => {
                packet::parse_packet(packet).map_err(OpusError::from)?;
                self.decoder.last_packet_duration()
            }
            Request::Packet(packet) => packet::parse_packet(packet)
                .map_err(OpusError::from)?
                .samples_per_channel(self.decoder.fs_hz()),
        };
        let needed = samples_per_channel_hint * self.decoder.channels() as usize;
        if pcm.len() < needed {
            return Err(OpusError::BufferTooSmall);
        }

        if self.float_scratch.len() < needed {
            self.float_scratch.resize(needed, 0);
        }

        let mut scratch = std::mem::take(&mut self.float_scratch);
        let result = self.run(request, &mut scratch[..needed]);
        self.float_scratch = scratch;
        let samples_per_channel = result.map_err(OpusError::from)?;
        let written = samples_per_channel * self.decoder.channels() as usize;
        for (dst, src) in pcm.iter_mut().zip(self.float_scratch[..written].iter()) {
            *dst = f32::from(*src) / I16_FULL_SCALE;
        }

        Ok(samples_per_channel)
    }

    /// Vuelve al estado recién creado, por ejemplo tras un salto en el flujo.
    pub fn reset(&mut self) {
        self.decoder.reset();
    }

    /// Estado final del decodificador de rango tras el último paquete, el que libopus da con
    /// `OPUS_GET_FINAL_RANGE` para comprobar la conformidad.
    pub fn final_range(&self) -> u32 {
        self.decoder.final_range()
    }

    /// Particiones recursivas de la última trama CELT (para depurar).
    #[doc(hidden)]
    pub fn last_split_count(&self) -> usize {
        self.decoder.last_split_count()
    }

    /// Memoria del filtro de deénfasis del canal 0 (para depurar).
    #[doc(hidden)]
    pub fn deemph_mem(&self) -> f32 {
        self.decoder.deemph_mem()
    }

    /// Si la última trama CELT era transitoria (para depurar).
    #[doc(hidden)]
    pub fn last_is_transient(&self) -> bool {
        self.decoder.last_is_transient()
    }

    /// Si el último paquete llevaba redundancia CELT en una trama SILK (para depurar).
    #[doc(hidden)]
    pub fn last_had_redundancy(&self) -> bool {
        self.decoder.last_had_redundancy()
    }

    /// Si esa redundancia era de transición de CELT a SILK (para depurar).
    #[doc(hidden)]
    pub fn last_celt_to_silk(&self) -> bool {
        self.decoder.last_celt_to_silk()
    }
}

/// Si `packet` trae información de corrección de errores de la banda (FEC: tramas LBRR de SILK)
/// con la que recuperar el paquete anterior, si se perdió. Es lo que consulta `opus_demo` de
/// libopus antes de pedir la recuperación (`opus_packet_has_lbrr`). Los paquetes solo CELT nunca
/// la traen.
///
/// # Errores
///
/// `InvalidPacket` si el paquete no es válido.
///
/// # Examples
///
/// ```rust
/// // Configuración 1 (SILK de banda estrecha, 20 ms), mono: el segundo bit alto de la trama es
/// // el indicador LBRR.
/// assert!(copus_decoder::packet_has_lbrr(&[0x08, 0b0100_0000])?);
/// assert!(!copus_decoder::packet_has_lbrr(&[0x08, 0b1000_0000])?);
/// # Ok::<(), copus_decoder::OpusError>(())
/// ```
pub fn packet_has_lbrr(packet: &[u8]) -> Result<bool, OpusError> {
    packet::has_lbrr(packet).map_err(OpusError::from)
}

/// Errores de la API pública. Los mensajes de `Display` se conservan en inglés, como en la
/// versión original, porque hay usuarios que pueden depender de ellos.
#[derive(Debug, thiserror::Error)]
pub enum OpusError {
    /// El paquete está mal formado o es incoherente.
    #[error("invalid packet")]
    InvalidPacket,
    /// El decodificador ha llegado a un estado interno no previsto.
    #[error("internal error")]
    InternalError,
    /// La trama decodificada no cabe en el búfer de salida.
    #[error("buffer too small")]
    BufferTooSmall,
    /// Un argumento no es válido; lleva el nombre del parámetro.
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),
}

impl From<Error> for OpusError {
    fn from(value: Error) -> Self {
        match value {
            Error::InvalidSampleRate(_) => Self::InvalidArgument("sample_rate"),
            Error::InvalidChannels(_) => Self::InvalidArgument("channels"),
            Error::BadPacket => Self::InvalidPacket,
            Error::OutputTooSmall { .. } => Self::BufferTooSmall,
            Error::NotImplemented => Self::InternalError,
        }
    }
}

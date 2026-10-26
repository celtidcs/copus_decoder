# Manual de uso

## 1. Añadir la dependencia

Copus_Decoder no se publica en crates.io; se usa desde git:

```toml
[dependencies]
copus_decoder = { git = "https://github.com/celtidcs/copus_decoder" }
```

Si vienes de `opus-decoder`, basta con cambiar la dependencia y sustituir `opus_decoder::` por
`copus_decoder::`: la API es la misma.

## 2. Decodificar un flujo

```rust
use copus_decoder::{OpusDecoder, OpusError};

fn decodificar_todo(paquetes: &[Vec<u8>]) -> Result<Vec<i16>, OpusError> {
    let canales = 2;
    let mut decodificador = OpusDecoder::new(48_000, canales)?;
    let mut trama = vec![0i16; decodificador.max_frame_size_per_channel() * canales];
    let mut salida = Vec::new();
    for paquete in paquetes {
        let muestras = decodificador.decode(paquete, &mut trama, false)?;
        salida.extend_from_slice(&trama[..muestras * canales]);
    }
    Ok(salida)
}
```

- El búfer de salida debe caber una trama completa: `max_frame_size_per_channel() * canales`.
  Si es menor, la llamada devuelve `OpusError::BufferTooSmall`.
- El valor devuelto son muestras **por canal**; en estéreo, el búfer tiene el doble, entrelazado
  (izquierdo, derecho, izquierdo, …).
- Para salida en coma flotante, `decode_float` con un `Vec<f32>`.

## 3. Paquetes perdidos

Cuando falta un paquete (pérdida en la red, hueco en el archivo), llama a `decode` con un paquete
vacío. El decodificador genera una trama de ocultación de la misma duración que la última
decodificada (si aún no ha decodificado ninguna, devuelve 0 muestras):

```rust
let muestras = decodificador.decode(&[], &mut trama, false)?;
```

## 4. Varios flujos: multiflujo

Dos flujos mono independientes en una salida estéreo (izquierdo el flujo 0, derecho el 1):

```rust
use copus_decoder::OpusMultistreamDecoder;

let mut decodificador = OpusMultistreamDecoder::new(48_000, 2, 2, 0, &[0, 1])?;
let mut pcm = vec![0f32; copus_decoder::OpusDecoder::MAX_FRAME_SIZE_48K * 2];
let muestras = decodificador.decode_float(&paquete_multiflujo, &mut pcm, false)?;
```

5.1 con dos flujos estéreo y dos mono, y el último canal en silencio:

```rust
let decodificador = OpusMultistreamDecoder::new(48_000, 6, 4, 2, &[0, 1, 2, 3, 4, 255])?;
```

El paquete multiflujo sigue el formato de RFC 6716, anexo B (todos los subpaquetes salvo el último
con delimitación propia).

## 5. Cambio de posición en el flujo

Tras un salto (búsqueda en un archivo) llama a `reset()` antes de decodificar el siguiente paquete,
para que el estado anterior no se mezcle con el nuevo punto.

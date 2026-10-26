# Historial de cambios

Cambios de Copus_Decoder respecto a `opus-decoder` 0.1.1. El historial de git conserva cada paso
por separado, empezando por el paquete original intacto.

## 0.1.0 — 2026-10-26

### Corrección

- **Eliminados los registros de depuración que escribían en disco.** La 0.1.1 contiene 29 regiones
  `#region agent log` que abren, en modo de añadir, un archivo de registro (22 con rutas absolutas
  de la máquina del autor bajo `/Users/tadeusz/Opus/Rasopus/.cursor/`, 7 con la ruta relativa
  `.cursor/debug-bea564.log`). Dos se ejecutan en cada trama CELT. Se quitan las 29
  ([`parches/0001-quitar-registros-de-depuracion.patch`](../parches/0001-quitar-registros-de-depuracion.patch));
  la salida del decodificador no cambia (conformidad RFC 8251 12/12 y red de equivalencia
  idéntica). Avisado al autor en el
  [issue #8](https://github.com/TadeuszWolfGang/Rusopus/issues/8).

### Proyecto

- El crate pasa a llamarse `copus_decoder`, con licencia GPL-3.0 o posterior sobre la rama
  Apache-2.0 del original; se conservan sus avisos y el de libopus en `licencias/`.
- Se quita la dependencia de desarrollo `criterion`, que solo usaban bancos no incluidos en el
  paquete.
- Red de equivalencia: `tests/red_equivalencia.rs` y `red/huellas.txt` (110 huellas generadas con
  la 0.1.1 original).
- Documentación en español y scripts de preparación del entorno con descarga verificada de los
  vectores de RFC 8251.

### Auditoría

En curso. Cada cambio de esta sección deja la red de equivalencia y la conformidad idénticas.

- Formato uniforme con `rustfmt`; `cargo fmt --check` queda limpio.
- Las pruebas de conformidad ya no escriben en disco: fuera las escrituras a un registro con ruta
  absoluta de la máquina del autor original, el volcado a `/tmp/rust_tv07.pcm` y las ocho variables
  de entorno de trazado.
- Fuera el código muerto de depuración de `src/`: la macro `debug_trace!`, las funciones de
  registro, los contadores de paquete que solo servían para trazar, una FFT de referencia sin
  usar y los `allow(dead_code)` que lo ocultaban. La DFT directa que la sustituye en las pruebas
  tiene su propia prueba unitaria.
- Fuera tres conversiones al mismo tipo; `cargo clippy --all-targets --all-features -- -D warnings`
  queda sin avisos.
- El comparador de salidas, que solo usan las pruebas, pasa de `src/compare.rs` a
  `tests/compare/`.
- Nueva prueba de robustez, `tests/no_panico.rs`: paquetes aleatorios, con TOC válido, truncados y
  con bits alterados, para todas las frecuencias y canales y en multiflujo. El decodificador debe
  devolver un error, nunca entrar en pánico. Encontró dos defectos, ya corregidos:
  - Con 16 bloques MDCT (tramas de 20 ms transitorias con cierto ajuste tiempo-frecuencia) la
    máscara de colapso de CELT se desbordaba. Pasa a 32 bits, como en libopus.
  - Un paquete híbrido que anunciaba más redundancia de la que cabía en la trama hacía entrar en
    pánico al decodificador. Ahora se descarta la redundancia, como hace libopus.
- No queda ningún `panic!`, `unreachable!`, `unwrap()` ni `assert!` fuera de las pruebas: las
  invariantes del orden LPC, la periodicidad LTP y la forma del tono las garantiza el tipo. Quedan
  12 `debug_assert!`, que solo actúan en compilación de depuración.
- Las pruebas encuentran los vectores de RFC 8251 sin configurar nada: por defecto los buscan en
  la carpeta que crean los scripts de preparación (`OPUS_TESTVECTORS_DIR` sigue sirviendo para
  usar otra). Antes apuntaban a `../../testdata`, una ruta del espacio de trabajo del autor original.
- Sin números mágicos en la lógica del decodificador: los valores con significado (frecuencias,
  duraciones, bandas, parámetros del decodificador de rango, de SILK y de CELT, valores iniciales
  del estado) son constantes con nombre, documentadas con su apartado de RFC 6716 o su función de
  libopus. Solo quedan como literal los desplazamientos de formato de coma fija, los índices de
  filtros desenrollados y los números que el propio RFC usa como nombre.
- Fuera las copias de un mismo valor o cálculo: una sola banda final de CELT por ancho de banda,
  una sola `BITRES`, un solo tipo de señal sonora de SILK y una sola lista de duraciones de trama.
  Las funciones reescritas tienen pruebas que las comparan con la versión anterior en todo su
  dominio.
- Los valores de la ocultación de pérdidas de SILK que no proceden de libopus quedan marcados en
  el código como no comprobados.
- Módulos de una sola responsabilidad, sin cambiar la salida (red de equivalencia y conformidad
  idénticas tras cada paso):
  - `lib.rs` queda como API pública; el decodificador de un flujo pasa a `decoder.rs`, partido
    por modos, y `OpusDecoder` ya no copia su estado.
  - CELT: la decodificación de una trama, la ocultación, el recorrido de bandas (uno solo para
    mono y estéreo), la asignación de bits (`rate.rs`) y la síntesis MDCT (`celt/synthesis.rs`)
    van por etapas; un solo postfiltro por canal y un tipo `CeltOutput` en lugar de un `bool`.
  - SILK: el estado de cada canal se agrupa en `SilkChannel`, el remuestreo tiene un solo camino
    y la síntesis, la lectura de índices y la de pulsos van por etapas.
  - Una sola copia de las primitivas de coma fija de SILK, del libro NLSF, de `ec_ilog` y de la
    saturación a `i16` (`pcm.rs`); el multiflujo comparte un solo camino para `i16` y `f32`.
  - `parse_packet` va por etapas, una por código de empaquetado.
  - Ninguna función de `src/` pasa de 100 líneas. El único `allow` de clippy de módulo que
    queda es `excessive_precision` en `celt/modes.rs`, cuyas tablas copian los literales de libopus.
- Pruebas unitarias del reinicio estéreo al pasar de mono a estéreo y de la entrada de una trama
  ocultada en la historia de SILK, conductas que la red de equivalencia no ejercita.
- Nueva barrera en `tests/multistream.rs`: la salida del multiflujo debe ser la de un
  decodificador por flujo repartida según el mapa de canales, en `i16` y en `f32`.
- Toda la documentación del código, en español. Los mensajes de error (`Display`) se conservan en
  inglés, como en la versión original.
- Las diferencias con libopus y las decisiones de diseño abiertas que se han encontrado quedan en
  [defectos conocidos](defectos-conocidos.md).

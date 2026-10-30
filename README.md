Copus_Decoder, decodificador Opus en Rust puro, corregido y auditado

Copus_Decoder es una copia corregida de opus-decoder 0.1.1 (https://crates.io/crates/opus-decoder/0.1.1),
el decodificador Opus (https://opus-codec.org/) en Rust puro de Tadeusz Wójcik, cuyo repositorio
original está en https://github.com/TadeuszWolfGang/Rusopus. Empezó siendo un parche sobre esa
versión, seguido de una auditoría de código que no cambiaba la salida; desde la versión 0.2.0 es
además un decodificador que se ha ido acercando, medida a medida, a libopus, la implementación de
referencia de Opus.

Sobre Celtilander

Soy un usuario novel en el mundo de la programación y la IA. He «ayudado a crear» esta app y espero
que a alguien le pueda servir para algo... sin más.

Lo que no funciona no lo escondo: está escrito en los defectos conocidos
(https://github.com/celtidcs/copus_decoder/blob/main/documentacion/defectos-conocidos.md). Si
encuentras algo que no esté ahí, cuéntamelo.

Para realizar esta app he montado un sistema colaborativo y de consenso entre ChatGPT Codex, Claude
Code y Gemini Antigravity. Yo puse la idea y dirigía el proyecto, mientras que ellos diseñaban,
repartían y verificaban el trabajo. Ningún código fue revisado por el agente que lo había escrito,
y una vez terminado todo debía ser aprobado por unanimidad. Hasta que no se conseguía el consenso,
no se daba una tarea por concluida. Cuando por algún motivo no se llegaba a un consenso, era yo
quien tomaba la decisión final, así que los fallos que pueda haber serán más míos que de ellos.

Saludos a todo el mundo.

Por qué existe esta copia

La versión 0.1.1 publicada en crates.io conserva instrumentación de depuración olvidada: 29
regiones marcadas como «region agent log» que abren, en modo de añadir, un archivo de registro.
Veintiuna usan la ruta absoluta /Users/tadeusz/Opus/Rasopus/.cursor/debug-bea564.log de la máquina
del autor, una usa /Users/tadeusz/Opus/Rasopus/.cursor/debug-9a4e1c.log y siete la ruta relativa
.cursor/debug-bea564.log, dentro del directorio actual del proceso. Casi todas están detrás de
guardas constantes y nunca se ejecutan, pero dos se ejecutan en cada trama CELT, ambas con la
primera ruta absoluta: postfilter_decoded, siempre, y postfilter_active_but_not_applied_in_rust,
cuando el postfiltro está activo. Si el proceso puede escribir en esa ruta, el decodificador
escribe en disco sin que nadie se lo pida; si no puede, lo intenta y falla en silencio en cada
trama. En Windows, una ruta que empieza por un solo separador se resuelve en la raíz de la unidad
actual, según la documentación de Microsoft
(https://learn.microsoft.com/es-es/dotnet/standard/io/file-path-formats). Se avisó al autor en el
issue 8 de su repositorio (https://github.com/TadeuszWolfGang/Rusopus/issues/8).

Esta copia quita las 29 regiones, con el parche que está en
parches/0001-quitar-registros-de-depuracion.patch, de modo que el decodificador ya no abre ningún
archivo. Demuestra después que la salida no cambia: los doce vectores oficiales de RFC 8251 siguen
pasando, y una red de huellas de la salida completa (seis combinaciones de frecuencia y canales,
pérdida de paquetes, paquetes dañados, salida de 16 bits y multiflujo) daba exactamente lo mismo
que la 0.1.1 original. Con esa red como garantía se auditó el código: sin valores incrustados,
refactorizado en piezas de una sola responsabilidad y siguiendo los principios SOLID. Desde
entonces, cada corrección que acerca la salida a libopus cambia a propósito solo las huellas
afectadas, y el historial de cambios dice cuáles y por qué; hoy la red tiene 111 huellas.

El historial de git conserva cada paso por separado, empezando por el paquete original intacto,
seguido del parche y después de la auditoría y las correcciones, para que cualquiera pueda
compararlos.

Qué hace

Decodifica paquetes Opus, según RFC 6716 con las correcciones de RFC 8251, a audio PCM. Admite los
tres modos de Opus (CELT, SILK e híbrido) y las transiciones entre ellos, con salida a 8, 12, 16,
24 o 48 kHz, en mono o en estéreo. Oculta los paquetes perdidos siguiendo a libopus en SILK, en
CELT y en el modo híbrido, y trata la transmisión discontinua (DTX). Recupera pérdidas con la
corrección de errores en banda (FEC) del paquete siguiente; en las tramas híbridas, solo su capa
SILK, como se explica en los defectos conocidos. Decodifica flujos multiflujo en el
formato que produce libopus, para 5.1, 7.1 o varios flujos mono independientes. Todo ello en Rust
puro, con el uso de código inseguro prohibido en todo el crate, sin FFI ni dependencias de C; su
única dependencia es thiserror.

No codifica, no lee contenedores como Ogg o WebM y no usa instrucciones SIMD. La salida en coma
flotante, decode_float, es por ahora la salida de 16 bits dividida entre 32 768, sin más
resolución; darle la de libopus es el primer hito de la versión 0.3.0.

Sin pérdidas y a 48 kHz en estéreo, la salida de los doce vectores oficiales difiere de sus
salidas de referencia como mucho en una unidad de 16 bits, salvo cuatro en el vector 08, algo que
no se oye. Lo que todavía no coincide con libopus, con sus cifras, está en los defectos
conocidos.

Lo que viene en la versión 0.3.0

La versión siguiente tiene seis hitos, en este orden. El primero es una salida en coma flotante
real: llevar a coma flotante todo el camino interno del decodificador, de la síntesis de CELT a los
fundidos entre modos y la suma de las capas de las tramas híbridas, para que decode_float tenga la
resolución de la de libopus y decode convierta a 16 bits una sola vez, al final. El segundo, que
depende del primero, es el recorte suave de libopus, que evita la distorsión de saturar en seco
cuando la señal pasa de la escala completa. El tercero es averiguar por qué dos ocultaciones de
pérdidas fuera de 48 kHz siguen lejos de libopus, con diferencias de hasta 296 y 156, y corregirlo
o demostrar que no se puede. El cuarto, explicar y cerrar los restos que quedan en los flujos de
prueba con tramas variadas, de hasta 24 y 17. El quinto, localizar por qué el multiflujo con
corrección de errores en banda difiere de libopus hasta 207. Y el sexto, conseguir que cada detalle
que hoy se comporta como libopus pero que ninguna prueba vigila tenga una prueba que lo distinga, o
dejar escrito por qué no es posible.

Cada hito tiene escrito qué debe quedar demostrado para darlo por alcanzado, y se trabajará con las
mismas reglas que esta versión: criterios antes de programar, una prueba que falle antes de cada
corrección y verificación en Windows y en Linux. El detalle está en documentacion/hoja-de-ruta.md.

Uso rápido

En el Cargo.toml de tu proyecto, la dependencia apunta a este repositorio, fijada a una versión:

    [dependencies]
    copus_decoder = { git = "https://github.com/celtidcs/copus_decoder", tag = "v0.2.1" }

Y para decodificar un paquete:

    use copus_decoder::{OpusDecoder, OpusError};

    fn decodificar(paquete: &[u8]) -> Result<usize, OpusError> {
        let mut decodificador = OpusDecoder::new(48_000, 2)?; // 48 kHz, estéreo
        let mut pcm = vec![0i16; decodificador.max_frame_size_per_channel() * 2];
        // Devuelve las muestras decodificadas por canal; un paquete vacío activa la ocultación.
        decodificador.decode(paquete, &mut pcm, false)
    }

La API es la del original con otro nombre de crate: donde ponía opus_decoder, ahora pone
copus_decoder. El manual de uso, en documentacion/manual-de-uso.md, lo explica paso a paso.

Dónde está cada cosa

La documentación vive en la carpeta documentacion. El índice, documentacion/README.md, presenta el
conjunto. Las funcionalidades, con sus límites y los errores que devuelve, están en
funcionalidades.md; cómo integrarlo como librería, con ocultación de pérdidas y multiflujo, en
manual-de-uso.md; cómo está organizado el código y por qué, en arquitectura.md. El historial de
cambios respecto al original está en historial-de-cambios.md, lo que no funciona o no está
comprobado en defectos-conocidos.md, y los hitos de la próxima versión en hoja-de-ruta.md. La
subcarpeta infraestructura reúne los requisitos, la guía de clonado, los guiones que preparan el
entorno y el guion que genera las referencias de libopus.

Cómo se verifica

Cuatro órdenes deben terminar sin errores ni avisos antes de aceptar cualquier cambio, en Windows y
en Linux:

    cargo test --all-features
    cargo clippy --all-targets --all-features -- -D warnings
    cargo fmt --check
    cargo audit

La conformidad con RFC 8251, la diferencia con las salidas de referencia y la red de equivalencia
necesitan además los vectores oficiales, que ocupan 75 MB comprimidos y no se incluyen en el
repositorio. Los guiones de documentacion/infraestructura los descargan, comprueban su SHA-256 y
preparan su manifiesto; la guía de clonado explica cómo ejecutar esas pruebas. Las pruebas frente
a libopus 1.6.1, con pérdidas, FEC y multiflujo, necesitan también sus salidas de referencia, que
en Linux genera documentacion/infraestructura/referencia-libopus/generar.sh compilando libopus y
comprobando el resultado con su manifiesto.

Licencia y atribución

Copus_Decoder se distribuye bajo la GNU General Public License versión 3 o posterior, cuyo texto
está en el archivo LICENSE.

El código original es de 2026, de Tadeusz Wójcik, con licencia doble MIT o Apache 2.0. Esta copia
lo recibe bajo la rama Apache 2.0, que la Apache Software Foundation declara compatible con la
GPLv3 (https://www.apache.org/licenses/GPL-compatibility.html). Sus textos de licencia se conservan
en la carpeta licencias, y el archivo AVISO.md indica qué se ha modificado, como exige el apartado
4(b) de la licencia Apache.

El original es a su vez un port de libopus (https://gitlab.xiph.org/xiph/opus), de Xiph.Org y otros,
con licencia BSD de tres cláusulas, cuyo aviso se conserva en licencias/LIBOPUS-COPYING-BSD-3. Opus
está sujeto a las licencias de patentes gratuitas que se citan en ese archivo.

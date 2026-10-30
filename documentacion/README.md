Documentación de Copus_Decoder

Copus_Decoder es una copia corregida y auditada de opus-decoder 0.1.1, un decodificador Opus según
RFC 6716 y RFC 8251 escrito en Rust puro, sin código inseguro ni FFI. La razón de ser de la copia y
su licencia están en el README de la raíz del repositorio.

Esta carpeta reúne todo lo necesario para usar el decodificador y para seguir desarrollándolo. Si
solo quieres usarlo, empieza por el manual de uso (manual-de-uso.md), que lleva de la dependencia a
la decodificación de un flujo con pérdidas o multiflujo, y consulta las funcionalidades
(funcionalidades.md) para saber qué parámetros acepta, qué devuelve y qué errores puede dar.

Si vas a tocar el código, la arquitectura (arquitectura.md) explica cómo está organizado, por qué
sigue la estructura de libopus y qué comprueba cada batería de pruebas. El historial de cambios
(historial-de-cambios.md) cuenta, versión a versión, qué se ha cambiado respecto al original y cómo
se midió. Los defectos conocidos (defectos-conocidos.md) recogen, con cifras, lo que no funciona, lo
que no coincide con libopus y lo que no está comprobado. La hoja de ruta (hoja-de-ruta.md) fija los
hitos de la versión siguiente.

La subcarpeta infraestructura describe el entorno. La visión general (infraestructura/vision-general.md)
enumera los componentes y los datos de prueba; los requisitos (infraestructura/requisitos.md), las
herramientas y versiones necesarias; y la guía de clonado (infraestructura/guia-de-clonado.md) lleva
de un clon vacío a un decodificador compilado y verificado. Los guiones preparar-entorno.sh y
preparar-entorno.ps1 preparan el entorno en Linux o macOS y en Windows, y la carpeta
referencia-libopus contiene el guion que genera, en Linux, las referencias de libopus 1.6.1 con que
se comparan las pruebas.

Garantía de equivalencia

Ninguna modificación del código se acepta si no cumple tres condiciones. La primera, que las
pruebas, clippy sin un solo aviso, cargo fmt --check y cargo audit pasen en Windows y en Linux. La
segunda, que los doce vectores de RFC 8251 superen la comparación de calidad de opus_compare y que
su salida no se aleje de la de referencia más de lo medido (tests/diferencia_libopus.rs). La
tercera, que la red de equivalencia (tests/red_equivalencia.rs) dé las 111 huellas de
red/huellas.txt. Esas huellas se generaron con la 0.1.1 original, salvo las que cada corrección
cambia a propósito, que el historial de cambios enumera; una huella distinta significa que la
salida cambió en al menos una muestra.

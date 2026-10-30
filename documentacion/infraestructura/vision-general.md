Visión general de la infraestructura

Copus_Decoder es una librería de Rust sin servicios externos, sin red y sin configuración en tiempo
de ejecución. Lo único que viene de fuera son los datos de prueba.

El código fuente está en src y en tests, y forma el crate copus_decoder, en la edición 2024 de
Rust. Su única dependencia es thiserror, que da forma a los errores públicos. Las huellas de la red
de equivalencia están versionadas en red/huellas.txt. La compilación va a la carpeta target, la que
cargo usa por defecto, que no se versiona.

Los vectores oficiales de RFC 8251 no se versionan: los guiones de preparación los descargan de
opus-codec.org a material_secundario/vectores, comprobando su SHA-256. Las referencias de libopus
1.6.1 tampoco: son opcionales y las genera, en Linux, el guion
documentacion/infraestructura/referencia-libopus/generar.sh, que las deja en
material_secundario/vectores/referencia-libopus-1.6.1 y las comprueba con su manifiesto.

No hace falta ninguna variable de entorno para compilar, usar la librería ni pasar las pruebas. Las
pruebas que necesitan los vectores los buscan en material_secundario/vectores/opus_newvectors, o en
la carpeta que indique OPUS_TESTVECTORS_DIR. Las de tests/referencia_libopus.rs buscan las
referencias de libopus en su carpeta por defecto, o en la que indique OPUS_REFERENCIA_LIBOPUS_DIR.

Las puertas de calidad (pruebas, clippy, formato y cargo audit) se ejecutan en Windows y en Linux
antes de aceptar cualquier cambio.

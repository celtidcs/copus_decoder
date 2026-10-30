Guía de clonado

Estos son los pasos para tener Copus_Decoder compilado y verificado en una máquina nueva.

Requisitos

Instala Rust desde https://rustup.rs. El documento de requisitos (requisitos.md) detalla las
versiones y el resto de herramientas.

Clonar

    git clone https://github.com/celtidcs/copus_decoder.git
    cd copus_decoder

Preparar el entorno

El guion de preparación comprueba las herramientas, instala clippy, rustfmt y cargo-audit si
faltan, descarga los vectores de RFC 8251 comprobando su SHA-256, escribe su manifiesto, compila y
pasa las pruebas. Se puede repetir cuantas veces se quiera sin efectos secundarios. En Linux o
macOS:

    bash documentacion/infraestructura/preparar-entorno.sh

En Windows, desde PowerShell:

    powershell -ExecutionPolicy Bypass -File documentacion\infraestructura\preparar-entorno.ps1

Verificar

Las cuatro puertas que debe pasar cualquier cambio son estas:

    cargo test --all-features
    cargo clippy --all-targets --all-features -- -D warnings
    cargo fmt --check
    cargo audit

La conformidad con RFC 8251, la diferencia con la salida de referencia y la red de equivalencia se
ejecutan en modo release y tardan unos minutos:

    cargo test --release --test conformance_rfc -- --ignored rfc_conformance_vectors
    cargo test --release --test diferencia_libopus -- --ignored
    cargo test --release --test red_equivalencia -- --ignored

Las pruebas buscan los vectores en material_secundario/vectores/opus_newvectors, que es la carpeta
que crean los guiones de preparación. Para usar otra, define OPUS_TESTVECTORS_DIR con su ruta.

La barrera de fuzzing, tests/fuzzing.rs, corre una ronda corta dentro de cargo test. Su campaña
larga muta además los paquetes de los vectores y de las referencias de libopus, y su número de
pasos se elige con la variable COPUS_FUZZ_ITERACIONES, que por defecto vale 300 000:

    COPUS_FUZZ_ITERACIONES=10000000 cargo test --release --test fuzzing -- --ignored --nocapture

La semilla es fija, así que cada ejecución recorre los mismos casos; un fallo se informa por clase,
con un ejemplo que dice la secuencia y el paso que lo reproducen.

Fuzzing guiado por cobertura, opcional y en Linux

La carpeta fuzz es un espacio de trabajo aparte para cargo-fuzz, que usa libFuzzer y necesita Rust
nightly; cargo test no la compila. Tiene dos objetivos. decodificar prueba OpusDecoder, con decode
y decode_float a la vez, en las diez configuraciones de salida, y multiflujo prueba
OpusMultistreamDecoder en cuatro topologías. Los dos comprueban, además de que no haya pánicos, que
decode y decode_float den lo mismo y que nada se escriba fuera de la salida. El formato de la
entrada está descrito en fuzz/fuzz_targets/comun.rs. Desde la raíz del repositorio:

    rustup toolchain install nightly --profile minimal
    cargo install cargo-fuzz --locked
    cargo +nightly fuzz run decodificar -- -max_total_time=3600
    cargo +nightly fuzz run multiflujo -- -max_total_time=3600

El corpus se guarda en fuzz/corpus y los fallos en fuzz/artifacts, dos carpetas que git ignora.

Pruebas contra libopus, opcionales y en Linux

tests/referencia_libopus.rs compara la salida con la de libopus 1.6.1 en flujos con pérdidas, con
FEC en banda (también con pasos de estéreo a mono), con DTX, en dos secuencias de ocultación y en
multiflujo, a 48 kHz en estéreo y a las demás frecuencias y canales. Sus referencias se generan
con un guion que descarga libopus comprobando su SHA-256, lo compila sin las aproximaciones rápidas
de coma flotante en target/referencia-libopus, genera los flujos y las salidas y los compara con
el manifiesto SHA256SUMS. Necesita gcc, make y curl, además de los vectores de RFC 8251, y se puede
repetir: si las referencias ya están, no hace nada.

    bash documentacion/infraestructura/referencia-libopus/generar.sh
    cargo test --release --test referencia_libopus -- --ignored

Las referencias quedan en material_secundario/vectores/referencia-libopus-1.6.1; para usar otra
carpeta, define OPUS_REFERENCIA_LIBOPUS_DIR antes de las dos órdenes. Se generaron en Linux x86-64.
libopus elige las instrucciones SSE o AVX al ejecutarse, y no está comprobado que otra CPU dé los
mismos bytes; si no los da, el guion lo dice y termina con error.

Usarlo desde otro proyecto

El manual de uso (documentacion/manual-de-uso.md) explica cómo añadirlo como dependencia y
decodificar.

Requisitos

Para compilar hace falta Rust, con rustc y cargo, en la versión 1.85 o posterior, que es la que
declara Cargo.toml; está comprobado con la 1.99.1. Se instala desde https://rustup.rs.

Para las puertas de estilo hacen falta clippy y rustfmt, los de la cadena instalada, que se añaden
con rustup component add clippy rustfmt. Para revisar las vulnerabilidades de las dependencias hace
falta cargo-audit, en la versión 0.22 o posterior, que se instala con cargo install cargo-audit
--locked.

Para descargar y extraer los vectores de prueba bastan curl y tar en Linux o macOS, o PowerShell
5.1 y tar en Windows 10 o posterior. Ocupan unos 200 MB: 75 MB comprimidos y 121 MB extraídos, a
los que se suma la compilación.

Solo para generar las referencias de libopus 1.6.1, que son opcionales, hacen falta en Linux gcc,
make, curl, tar y sha256sum, y unos 460 MB más de disco: 390 MB de referencias y 70 MB de libopus
compilada. La guía de clonado explica cuándo merece la pena.

Las puertas se ejecutan en Windows 11 y en Linux, ambos en x86-64. No hace falta GPU, ni red salvo
para descargar los vectores, ni privilegios de administrador.

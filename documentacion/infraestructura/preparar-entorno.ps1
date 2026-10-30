# Prepara un entorno Windows para compilar y verificar Copus_Decoder.
# Idempotente: se puede ejecutar varias veces; lo que ya está hecho no se repite.
#
# Uso (desde cualquier carpeta):
#   powershell -ExecutionPolicy Bypass -File documentacion\infraestructura\preparar-entorno.ps1
$ErrorActionPreference = 'Stop'

# Raíz del proyecto: dos niveles por encima de este script.
$Raiz = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
Set-Location $Raiz

# Versión mínima de Rust que declara Cargo.toml (rust-version).
$RustMinima = [version]'1.85'
# Vectores oficiales de conformidad de RFC 8251 y su SHA-256 conocido.
$UrlVectores = 'https://opus-codec.org/static/testvectors/opus_testvectors-rfc8251.tar.gz'
$ShaVectores = '6b26a22f9ba87b2b836906a9bb7afec5f8e54d49553b1200382520ee6fedfa55'
# Carpeta local, fuera del control de versiones (material_secundario/ está en .gitignore).
$CarpetaVectores = Join-Path $Raiz 'material_secundario\vectores'
$TarVectores = Join-Path $CarpetaVectores 'opus_testvectors-rfc8251.tar.gz'
$DirVectores = Join-Path $CarpetaVectores 'opus_newvectors'
$NumeroDeVectores = 12

function Paso($texto) { Write-Host "`n== $texto" }
function Falla($texto) { throw "ERROR: $texto" }
# Ejecuta un programa externo y se detiene si termina con error.
function Ejecutar { & $args[0] $args[1..($args.Count - 1)]; if ($LASTEXITCODE -ne 0) { Falla "falló: $args" } }

Paso '1. Sistema y herramientas'
Write-Host ([System.Environment]::OSVersion.VersionString)
foreach ($herramienta in 'cargo', 'rustc', 'rustup', 'tar') {
    if (-not (Get-Command $herramienta -ErrorAction SilentlyContinue)) {
        Falla "falta '$herramienta' (Rust: https://rustup.rs)"
    }
}
$versionRust = [version](((rustc --version) -split ' ')[1] -replace '-.*$', '')
Write-Host "rustc $versionRust (mínima $RustMinima)"
if ($versionRust -lt $RustMinima) { Falla "rustc $versionRust es anterior a ${RustMinima}: ejecuta 'rustup update'" }

Paso '2. Componentes de Rust y cargo-audit'
Ejecutar rustup component add clippy rustfmt
cargo audit --version *> $null
if ($LASTEXITCODE -ne 0) { Ejecutar cargo install cargo-audit --locked }

Paso '3. Carpeta de los vectores'
New-Item -ItemType Directory -Force $CarpetaVectores | Out-Null

Paso '4. Vectores de RFC 8251'
if (-not (Test-Path $TarVectores)) {
    Invoke-WebRequest -Uri $UrlVectores -OutFile "$TarVectores.parcial" -UseBasicParsing
    Move-Item "$TarVectores.parcial" $TarVectores
}
$shaReal = (Get-FileHash -Algorithm SHA256 $TarVectores).Hash.ToLower()
if ($shaReal -ne $ShaVectores) { Falla "SHA-256 inesperado de $TarVectores ($shaReal); bórralo y repite" }
if (-not (Test-Path $DirVectores)) { Ejecutar tar -xzf $TarVectores -C $CarpetaVectores }
# Manifiesto que leen las pruebas: los 12 vectores, comparados en estéreo a 48 kHz.
$lineas = 1..$NumeroDeVectores | ForEach-Object {
    $n = '{0:D2}' -f $_
    "testvector$n testvector$n.bit testvector$n.dec 48000 2"
}
[System.IO.File]::WriteAllText((Join-Path $DirVectores 'vectors.txt'), (($lineas -join "`n") + "`n"))
Write-Host "vectores listos en $DirVectores"

Paso '5. Compilación'
Ejecutar cargo build --release --all-targets

Paso '6. Comprobación final'
Ejecutar cargo test --release --all-features

Paso 'Listo'
Write-Host @"
Pruebas con los vectores (conformidad RFC 8251, diferencia frente a libopus y red de equivalencia):
  `$env:OPUS_TESTVECTORS_DIR = '$DirVectores'
  cargo test --release --test conformance_rfc -- --ignored rfc_conformance_vectors
  cargo test --release --test diferencia_libopus -- --ignored
  cargo test --release --test red_equivalencia -- --ignored
"@

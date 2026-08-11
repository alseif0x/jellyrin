# Plan integral de optimización de Jellyrin: base de datos, catálogo y FFmpeg

## Estado de ejecución (iniciado 2026-08-07; actualizado 2026-08-10)

Este bloque describe el árbol de trabajo actual, todavía sin publicar. El
diagnóstico de la sección 2 se conserva deliberadamente como línea base
histórica y no debe confundirse con este estado vigente.

- El cierre de seguridad en curso actualiza el mínimo a Rust 1.94 y SQLx 0.9.
  `Cargo.lock` ya no contiene `rsa 0.9.10`; SQLx conserva metadata lock-only de
  su crate MySQL opcional, pero no lo compila ni arrastra RSA. La API, el driver
  PostgreSQL y el adaptador SQLite explícito compilan con `--all-features`.
  MySQL continúa siendo solo una reserva de arquitectura, sin enlazar su driver.
- El siguiente candidato FFmpeg fija la revisión oficial
  `1e0279143db99d7324b17f9784b3229122269b38` y su archivo por SHA-256. El build
  verifica, también por SHA-256, los 16 parches oficiales HIGH que NVD asocia a
  8.1.2 y demuestra con `git apply --reverse --check` que ya forman parte de la
  fuente. La etapa FFmpeg AArch64 compila y conserva cero encoders y solo AAC.
  El gate reemplaza el matcher genérico no concluyente de Trivy por consulta CPE
  exacta a NVD y falla cerrado ante cualquier HIGH/CRITICAL nuevo no mapeado.
  La imagen completa AArch64, corpus y SBOM ya están verdes sobre `8026d7f`; ese
  scan exacto dio RustSec=0 y NVD-FFmpeg=0, pero el runtime Debian conservaba 13
  CVE únicas/22 ocurrencias. El candidato posterior reemplaza solo la etapa
  productiva por `cc-debian13:nonroot` fijado por digest: inventaría 13 paquetes,
  ejecuta FFmpeg/ffprobe durante el build, pasa el corpus real y da 0 hallazgos
  HIGH/CRITICAL en Trivy. El commit exacto `6a15aec579b8` ya repitió corpus,
  SBOM, RustSec, NVD y Trivy con todos los gates en cero. El smoke histórico de
  ese commit aplica sus 13 migraciones, arranca la imagen read-only como `10001:10001`,
  valida health/readiness y cierre limpio contra PostgreSQL desechable. Falta
  validar AMD64; QEMU 8.2.2 del host ARM falla internamente durante la
  compilación, por lo que CI exige ahora una imagen AMD64 nativa.
  Esta evidencia pertenece a `6a15aec`/`8026d7f`, no al HEAD actual. El HEAD
  conserva RustSec real en cero, pero debe reconstruir imagen, corpus, SBOM,
  NVD y Trivy en AArch64 y repetirlos en AMD64 nativo antes de promoción.

- La costura de selección de persistencia ya tiene `DatabaseDriver` con
  `PostgreSql`, `Sqlite` y `MySql`, además de `DatabaseConfig` y una única
  factory `DatabaseManager`. No existe todavía intercambio transparente de
  motores: `postgresql`
  —y su alias `postgres`— es el único driver productivo; `sqlite` es el selector
  público canónico del adaptador real usado en tests/migración y
  `sqlite-legacy` queda como alias compatible; `mysql` se reconoce como reserva
  futura. SQLite y MySQL fallan antes de conectar cuando se solicitan al manager
  productivo: no existe fallback silencioso.
- Dos contratos piloto de dominio, `MediaCatalogStore` y `XtreamCatalogStore`,
  demuestran la portabilidad con repositorios, SQL y migraciones nativos por
  dialecto; muchas operaciones siguen siendo métodos inherentes. No se usa `AnyPool` ni
  SQL compartido condicionado por driver. `jellyrin-api` ya no contiene SQLx,
  pools ni consultas directas; `ProductionDatabase` sigue siendo hoy un alias
  de `PostgresDatabase`, por lo que añadir MySQL exige implementar todo el
  contrato y su suite, no solo otra rama de selección.
- PostgreSQL ya es el backend del servidor: esquema nativo en migraciones
  separadas, repositorios portados, pools API/worker, roles runtime/migrator,
  timeouts, checksum de esquema y readiness. El grafo normal de
  `jellyrin-server` no enlaza SQLite. El feature público `sqlite` y su alias de
  compilación `legacy-sqlite` mantienen el adaptador SQLite fuera del runtime
  productivo.
- `jellyrin-migrate` conserva SQLite como fuente legacy y migra las tablas
  duraderas con preflight, dry-run, UUID estables y digest origen/destino. El
  delta post-vault incorpora `provider_secrets` y sus bytes
  BLOB↔`bytea`, porque las configuraciones protegidas referencian su
  `secret_id`. El cierre local pasa 36/36 pruebas y clippy estricto; cubre bytes
  no UTF-8, digest tipado y preflight de referencias plugin/tuner/Live TV. Los
  catálogos reconstruibles, cachés, sesiones activas, historial operacional de
  sync y transcodes se recrean. El fixture completo ya pasó contra PostgreSQL
  real y comparó BLOB→`bytea` byte a byte. El cierre final de
  `jellyrin-db` cubre baseline, repositorios, snapshots/no-op, vault y telemetría:
  pasan 169 pruebas aprobadas, 0 fallidas y 4 ignoradas más su doctest contra el árbol actual, con PostgreSQL real
  donde corresponde.
- El hot path compatible de `/Items` y `/Users/{id}/Items` ya usa el contrato
  `MediaCatalogStore`: `COUNT(*)` exacto y página `LIMIT/OFFSET` en el mismo
  snapshot `REPEATABLE READ`, filtros y orden estables en SQL, user data por
  `LEFT JOIN` y cap duro de 500. `ParentId` entra en ese pushdown cuando es una
  carpeta virtual real; `Recursive=true` incorpora sus carpetas hijas. Los
  padres inválidos, Series/Season o nodos sintéticos conservan el camino legacy
  para no cambiar semántica.
- `/Items/Counts` y `/Users/{id}/Items/Counts` usan el mismo gate neutral, pero
  ignoran paginación y orden: calculan tipos base en SQL y, dentro del mismo
  snapshot, streaméan solo episodios o los seis subárboles metadata relevantes.
  Series, artistas, álbumes y trailers se deduplican/cuentan en memoria
  O(series + facetas distintas) con la semántica ASCII/Unicode exacta del
  fallback. Búsqueda y comparaciones sensibles a collation, más filtros no
  modelados, permanecen deliberadamente en el fallback legacy.
- El pushdown general no está terminado. Una petición sin `Limit`, filtros aún
  no modelados —personas, estudios, tags, ratings, años o premiere—,
  jerarquías Series/Season y shapes complejos de próximos episodios pueden seguir
  materializando catálogo y filtrar/ordenar en Rust. El shape común de Upcoming
  ya usa una proyección temporal indexada. Resume con `Limit` y sin
  predicados adicionales ya aplica policy, count y página en SQL; su fallback
  complejo o sin límite conserva la materialización legacy. Sugerencias ya usa
  el catálogo SQL para los shapes compatibles y `/Search/Hints` dispone ahora
  de una página SQL máxima de 100 con total exacto. Su scope especializado solo
  busca nombre, álbum, artista de álbum, serie y artistas; Overview, claves JSON
  e IDs internos no se convierten accidentalmente en hints. Los shapes de
  Series/Season y filtros complejos conservan exactamente el fallback legacy.
  Por eso el cap 500 protege el camino SQL, pero todavía no constituye una
  garantía global de memoria para toda la API.
- `GenreIds` ya dispone de una proyección exacta y un `EXISTS` indexado para
  `/Items` y `/Users/{id}/Items` paginados. Acepta nombre, ID estable e ID
  importado con semántica OR, normaliza/deduplica y rechaza más de 64 selectores
  distintos con 400 en vez de activar el fallback O(N). El caso `ParentId` no
  recursivo conserva todavía el camino legacy porque ese camino expande hijos;
  `Recursive=true` sí tiene scope SQL equivalente.
- `PersonIds`, `StudioIds` y `Tags` usan la proyección exacta
  `media_item_filter_selectors`. Conserva la precedencia legacy
  `People/Cast` sobre `SeriesPeople`, IDs importados incluso en objetos sin
  `Name`, nombre raw e ID estable para personas/estudios, y tags raw sin aliases
  inventados. Cada clase usa OR interno y las clases se combinan con AND. Los
  cuatro grupos de selector se normalizan/deduplican y tienen cap independiente
  de 64 antes de elegir SQL o fallback. `Episode+PersonIds` mantiene el camino
  legacy por su orden de serie/temporada/episodio, y `ParentId` no recursivo lo
  conserva por la expansión de descendientes.
- La migración 108 normaliza las facetas de catálogo usadas por colecciones y
  lookups (`Genre`, `MusicGenre`, artistas, álbum, personas, estudios, tags y
  año) en `media_item_facets`/`media_item_facet_aliases`. Snapshot, update y
  rebuild mantienen esas tablas atómicamente en PostgreSQL y SQLite. La
  migración PostgreSQL 109 añade un marker de versión: upgrade y migración
  SQLite→PostgreSQL reconstruyen la proyección una sola vez dentro de la misma
  transacción; readiness rechaza marker ausente, viejo o futuro. Un segundo
  `schema` es O(1) y no cambia `completed_at` ni `xmin`. La migración 110 añade
  `media_item_genre_selectors(selector,item_id)` y eleva el extractor a v2. La
  migración 111 añade `media_item_upcoming_dates` con segundos+nanos exactos y
  eleva el extractor a v3; evita casts laxos de JSON y pérdida sub-microsegundo.
  La 112 añade los selectores exactos Person/Studio/Tag y eleva el extractor a
  v4. Snapshots, updates y escaneos NFO actualizan las cuatro proyecciones dentro de la
  misma transacción. SQLite invalida/reconstruye una sola vez su proyección
  legacy y rechaza versiones futuras en lugar de degradarlas silenciosamente.
- Las colecciones de metadata y sus rutas by-name/by-ID usan ahora esas facetas
  indexadas para los shapes globales o de carpeta equivalentes. `/Items/Filters`
  y `Filters2` tienen un contrato set-based separado, sin `LIMIT`, que reutiliza
  los mismos predicados de catálogo y expande únicamente las claves JSON y
  streams exactos del contrato Jellyfin. Los tests SQLite, PostgreSQL real y API
  incluyen más de 500 filas realmente seleccionadas. Predicados complejos,
  parents sintéticos y Live TV conservan el fallback; no se mezclan
  `SeriesGenres`, `Albums`, `Cast` ni `SeriesStudios` donde el endpoint legacy
  no los exponía.
- La migración 117 elimina la expansión JSON recursiva del hot path de
  `/Items/Filters`: mantiene una proyección exacta y versionada por item y
  carpeta, con candidates source-aware, FKs compuestas, invalidación y fallback
  fail-closed. El publish de snapshots es diferencial y un no-op conserva
  `xmin`/`completed_at`; PostgreSQL usa el índice de carpeta para un scope único
  y probes por item para scopes globales o múltiples, con `jit=off` y
  `work_mem=32MB` limitados a la transacción interactiva.
- El P0 separa por contrato las familias que consume cada endpoint:
  `/Items/Filters` no calcula idiomas y `Filters2` no calcula años, ratings,
  personas ni escalares. Los escalares dejaron además de entrar en el
  `row_number()` masivo y PostgreSQL los obtiene en una sola agregación. No se
  pagina ni se truncan valores. `Filters2` devuelve géneros `{Name,Id}` e idiomas
  `{Name,Value}`, y `Filters` devuelve años numéricos como Jellyfin Web espera.
  En el benchmark aislado de 455.520 episodios + 39.093 películas, el modo
  superset queda en p95 0,900 s/4 concurrentes 2,090 s y `Filters2` en p95
  0,700 s/4 concurrentes 1,814 s, ambos con 0 bytes temporales. En staging,
  Movies/Filters2 responde en 0,618 s, pero Series/Episode todavía tarda
  11,1–11,8 s con el binario desplegado de esquema 117.
- El esquema 118 implementa el fast path sin paginar ni truncar: resume
  por carpeta y tipo efectivo únicamente los valores distintos y conserva el
  ganador determinista exacto por nombre de item, UUID, prioridad y posición de
  origen. Coverage se publica al final de la misma transacción, se invalida ante
  cualquier cambio y se valida item por item; tombstones, sources incompletos o
  conteos compensados no pueden habilitarlo. La puerta acepta por ahora una
  carpeta y un único tipo Movie/Episode sin predicados adicionales; todo shape
  más complejo vuelve a la ruta 117 exacta. Sobre una copia recién restaurada
  del catálogo real, el ensayo final 117→118 tardó 17,183 s; en staging
  productivo tardó 11,528 s y condensó 494.613 items/989.226 contribuciones en
  96 filas (66 Movie, 30 Episode). La lectura aislada completa
  queda en p50 2,810 ms, p95 3,058 ms, cuatro concurrentes 8,099 ms y 0 bytes
  temporales. Ya desplegado, las llamadas HTTPS calientes quedan en 38–40 ms.
  La carga mixta 400/16 terminó con cero fallos/deadlocks/temporales y p95
  Movies Filters/Filters2 0,225/0,226 s y Series 0,227/0,222 s.
- El esquema 119 mantiene ese resumen vigente tras updates puntuales sin
  reconstruir la biblioteca completa. Cada carpeta tiene una revisión fuente
  monotónica; coverage solo se lee cuando revisión fuente, reconciliada y
  publicada coinciden. Los writers controlados capturan la revisión inicial,
  aplican un único bump CAS bajo bloqueo y recalculan únicamente los buckets
  multivalor antiguos/nuevos y los escalares modificados. Un writer externo,
  una discrepancia de contadores o una carrera fuerza rebuild exacto dentro de
  la transacción; cualquier estado intermedio carece de coverage y usa la ruta
  117 fail-closed. La normalización y deduplicación permanecen en PostgreSQL,
  incluido `lower()` de containers, y updates idénticos no avanzan revisión ni
  generan WAL de resumen. Los tests cubren dos contributors, cambio de grafía,
  no-op y una carrera reproducible fuera del advisory lock. En una copia real,
  118→119 tardó 408 ms y dejó 2/2 carpetas reconciliadas y cero coverage stale;
  en staging tardó 438 ms y el segundo `schema` 386 ms. Un `PlaybackInfo`
  Xtream real avanzó la revisión Movie 0→2 por sus dos escrituras, mantuvo
  `reconciled=2`, una coverage vigente y cero procesos FFmpeg. La carga HTTPS
  20×4 a concurrencia 8 dio p95 0,111–0,122 s para
  Movies/Series × Filters/Filters2.
- El esquema 120 elimina el bypass basado en GUC de la publicación del resumen.
  El rol runtime conserva solo lectura sobre valores, coverage y revisiones, y
  publica mediante funciones `SECURITY DEFINER` estrechas con `search_path`
  fijado; los wrappers de trigger siguen siendo `SECURITY INVOKER` para que una
  escritura ajena invalide siempre y falle cerrado. La reconciliación puntual
  valida exactamente la proyección anterior antes de reemplazarla, recalcula
  los ganadores escalares desde las fuentes reales y publica coverage/revisión
  en la misma transacción. Los lotes remotos reconstruyen mediante el mismo
  límite privilegiado cuando cambia su proyección. En PostgreSQL 16 aislado se
  verificaron rebuild, cambio `Drama`→`Action`, subtítulos, rechazo de una
  proyección anterior falsa, ACL de solo lectura y ataques con ambos GUC
  históricos y sombras temporales; todos quedaron fail-closed. La suite DB
  efectiva quedó verde tras repetir aisladamente un test de playback que había
  sufrido un deadlock únicamente durante la ejecución paralela.
- El siguiente lote local agrupa en una sola transacción la escritura de
  `media_info` y metadata que realiza el probe Xtream. Si la proyección derivada
  no cambia, no reconcilia ni avanza la revisión; si cambia, la sustituye y
  publica coverage una sola vez. Además, `Filters` y `Filters2` traducen el tipo
  sintético `Series` a su fuente persistida `Episode`, incluso en solicitudes
  mixtas `Movie,Series`. La sincronización no depende de paginación Xtream no
  estándar: descarga con límite a un temporal, parsea bloques de 500, hace
  append a staging y publica atómicamente. La navegación sí usa páginas SQL con
  total exacto; Series se agrupa mediante la proyección PostgreSQL y nunca carga
  los 455.520 episodios en memoria por petición.
- El commit `ad0e14d` está desplegado en staging con SHA-256
  `9ba49ca405de29b05e40ba0f909e54ab4719cbe6c50b5e660ff6f483ae37660a`.
  Health, readiness y HTTPS quedaron verdes, el servicio conserva cero
  reinicios, las 2/2 revisiones de filtros están reconciliadas, coverage tiene
  dos filas vigentes y no se inició FFmpeg. El rollback recuperable es
  `/var/backups/jellyrin/jellyrin-server-pre-ad0e14d-20260810T090845Z`, cuyo
  SHA-256 coincide con el binario anterior `cef61151…6069c2a`.
- DLNA ya no relee el catálogo completo por cada carpeta: browse/search usa
  `media_items_for_virtual_folders` y solo hidrata metadata para los IDs del
  dominio seleccionado. El detalle de carpeta reutiliza conteos SQL agrupados
  y los sidecars locales se consultan únicamente dentro de la carpeta virtual
  del item origen. El Browse raíz agrupa todas las carpetas en una sola
  consulta, eliminando su N+1. Instant Mix de audio queda acotado a la biblioteca
  del item fuente; segmentos y trailers cargan metadata solo por sus IDs,
  mientras Counts usa su proyección nativa y Search/Hints recibe la metadata
  dentro de su página SQL y evita una segunda consulta. El gate recovery bloquea la reintroducción de
  esos scans.
- Los snapshots remotos de películas y series se publican en una sola
  transacción, con advisory locks, staging durable por chunks, tombstones y
  rollback ante fallo tardío. La migración 113 añade ocho tablas reconstruibles
  para recibir hasta 1.000 items por append y 1.000.000 por biblioteca sin
  mantener una transacción abierta durante las llamadas HTTP. Movies y Series
  permanecen invisibles hasta que `publish` promueve ambas juntas; abort y GC
  eliminan generaciones incompletas. Un snapshot idéntico conserva `updated_at` y `last_seen_at` de
  las filas media: ya existe el no-op sin rewrites; se mantiene una fila de
  auditoría de sync. La carga continúa usando `QueryBuilder` por chunks, no
  `COPY FROM STDIN`.
  El runner comparativo real ya descarta cambiar esa ruta por intuición: COPY
  fue solo 1,046x más rápido a 100k y 1,030x a 500k en PostgreSQL local, muy
  lejos del gate 2x; la ruta productiva permanece simple y medida.
- La reproducción remota elige `DirectLocal`, `DirectProxy`, `HlsRemux` o
  `HlsTranscode` por stream. El proxy soporta Range y redirects HTTP explícitos
  con máximo de cinco saltos; reconstruye el cliente, resuelve y fija DNS, y
  revalida SSRF en cada destino. Bloquea HTTPS→HTTP y nunca registra URLs.
  Las fuentes de configuración continúan sin seguir redirects. H.264/AAC
  compatible evita encode; H.264 con audio no
  compatible convierte solo audio y vídeo incompatible puede convertir solo
  vídeo cuando el perfil lo requiere.
- `MaxStreamingBitrate`, `TranscodingProfiles` y `CodecProfiles` participan en
  `PlaybackInfo`, el plan FFmpeg y la identidad de deduplicación/seek. Además de
  width/height/bitrates, el árbol actual evalúa profile, level, bit depth, frame
  rate y audio channels, produce razones deterministas y mantiene un camino
  directo sin CPU cuando todos los streams cumplen. No sube FPS si la fuente ya
  cumple, no hace upmix si faltan metadatos de canales y acepta H.264 level 1.0.
- Los procesos HLS ya publican telemetría numérica acotada. En Linux una única
  lectura de `/proc/<pid>/stat` cada dos segundos obtiene CPU acumulada,
  porcentaje de CPU y RSS del líder, validando `starttime` contra reutilización
  de PID. VOD, Live y seek conservan además frame, `fps`, `speed` y posición en
  memoria; solo VOD mantiene su escritura SQL coalescida anterior. Un fallback
  borra la muestra del intento previo. ActiveEncodings expone el detalle sin
  argv/URL/stderr y Diagnostics solo agregados de cardinalidad fija.
  El cierre API/CodecProfiles y el rerun global están verdes.
- `JELLYRIN_FFMPEG_MODE=enabled|remux-only|disabled` es una política central.
  El default y el perfil recomendado para este host son `remux-only`; habilitar
  encode requiere una decisión explícita. Live HLS aplica esa política antes de
  clasificar el trabajo: en `remux-only` usa `Copy/Copy`, no incluye `libx264`
  ni AAC y consume el carril remux. El modo `enabled` también comienza con
  copy y, solo si el remux termina antes de producir un primer segmento no
  vacío, hace un único intento encode. El permiso remux se libera antes de
  solicitar el carril encode; stop, idle y cuota no activan el fallback. La
  misma política acotada cubre VOD continuo, seek bajo demanda y Live HLS,
  incluido el receptor broadcast legacy mediante `resubscribe()` sin duplicar
  el productor ni el lease. `remux-only` nunca cae a encode. Hay un cupo agregado para todos los
  hijos FFmpeg, además de cupos y cola acotada por vídeo, audio, remux, probe y
  auxiliares, con readrate, hilos/preset, rolling HLS, cuota, retención,
  watchdog, coalescing de progreso y cleanup. En encode, el presupuesto de
  hilos limita encoder y grafos de filtros simples/complejos; remux, copy y
  audio-only no reciben flags de filtros de vídeo.
- El siguiente candidato de runtime compila una revisión oficial FFmpeg posterior a 8.1.2,
  fijada por commit y SHA-256 con `--disable-everything`: no contiene encoders y su único
  decoder es AAC, necesario para recuperar el sample rate desde MPEG-TS antes
  de muxar HLS con stream copy. El corpus MP4/MKV/MPEG-TS pasa probe y remux HLS
  real. El clasificador incorpora ahora intención tipada no serializable del
  builder y validación CLI fail-closed: aliases globales, stream specifiers,
  filtros, codec implícito o comandos no construidos por el core se cargan como
  encode y quedan bloqueados en `remux-only`.
- Cada FFmpeg/ffprobe se ejecuta en su propio process group Unix. El stop normal
  envía `SIGTERM` al grupo, concede una gracia acotada, escala a `SIGKILL` y
  siempre espera/recolecta el hijo; drop, timeout y wrappers con descendientes
  también cierran el grupo. La captura de salida y cada línea de stderr están
  acotadas. Este hardening ya está implementado, no es trabajo pendiente.
- El shutdown del servidor detiene productores y cuerpos remotos, cancela
  transcodes, streams, grabaciones y hosts de plugin, y espera las tareas
  retenidas dentro del presupuesto de Compose/systemd. Queda pendiente medirlo
  con fuentes reales y comprobar en staging que no sobreviva ningún descendiente.
- MAGSTV mantiene una frontera opaca/JIT: catálogo sin URL, referencia de
  proveedor autenticada, resolución acotada y entrega por proxy seguro. Todo
  `ExternalProcess` con capacidad `LiveTvProvider` entra en la frontera aunque
  omita `ProviderSecrets`; omitirlo impide recibir el grant, no permite escribir
  secretos por configuración genérica. El core cifra primero, relee el estado
  canónico bajo un lock R/W por plugin y entrega un grant ligado a
  plugin/tuner/acción/id/revisión solo en un proceso one-shot. La detección de
  secretos cubre variantes de claves y URLs, las respuestas con grant usan
  canarios y los canales se proyectan a un esquema seguro. El entorno parte de
  `env_clear`: además de prohibir
  prefijos para este tipo de plugin, rechaza siempre URL/variables de DB, claves
  del vault y namespaces habituales de credenciales cloud/CI aunque el manifest
  los enumere explícitamente. También rechaza nombres exactos con forma de
  credencial de cuenta, conservando únicamente settings operativos revisados.
  La importación tiene deadline global de 120 segundos, 256 páginas, 100.000
  canales, 10.000 categorías, 1 MiB por RPC y un presupuesto agregado de 64 MiB
  de JSON codificado, y el
  browse Live TV usa PostgreSQL con total exacto y
  máximo 500. El parche del repositorio `jellyrin-plugin-magstv` para consumir y
  validar el grant y retirar el fallback de credenciales de cuenta por entorno
  ya está aplicado y validado contra el core local. El árbol integra
  `origin/main` `2700d7f` mediante el merge `43551fe`, añade la adaptación
  `ExternalProcess` local `8ce47b4` y fija 0.1.1 en `9596f1c`; el pin público
  todavía apunta a la revisión anterior. Staging refrescó el repositorio,
  instaló y activó 0.1.1, y su configuración admin responde sin devolver
  `Username` ni `Password`. Egress, secretos operativos y el E2E real siguen
  siendo gates. La clave de referencia no asociada a la cuenta ya se generó en el env
  root-only de staging; faltan todavía el perfil WireGuard de salida MX y los
  metadatos/secretos legítimos que no pueden inventarse desde el core.
- Xtream integrado persiste nuevas publicaciones Live TV con
  `ProviderReference` y VOD/episodios con `RemoteSourceRef`, sin URL. Resuelve la
  fuente JIT y entrega a FFmpeg/ffprobe un relay loopback opaco. Un
  `direct_source` solo se acepta si su URL normalizada es semánticamente igual a
  la reconstruida; una variante alternativa no crea otra ruta de egress. El
  vault guarda
  una sola copia de credenciales mediante AES-256-GCM, nonce aleatorio, envelope
  versionado, key id y AAD ligada a versión/key/provider/secret; los buffers y
  representaciones de debug se redactan o limpian.
  La sincronización VOD/Series descarga a un fichero temporal privado, analiza
  el array JSON incrementalmente en bloques de 500 y lo entrega al staging DB
  en lotes máximos de 1.000. El límite se aplica después de filtrar categorías;
  `MovieLimit=0` y `SeriesLimit=0` significan importar todo. El parser valida
  también el cierre y contenido restante del documento antes de publicar. Si el
  catálogo Series global supera 64 MiB, usa `category_id` sobre las categorías
  seleccionadas, exige que cada respuesta respete el filtro y deduplica IDs.
  El resultado de sync conserva los contadores legacy y añade observabilidad
  counts-only de recibidos, seleccionados, staged y publicados para Movies,
  Series y Episodes. Publicado solo aumenta después del publish atómico; los
  límites efectivos de bytes, items inspeccionados, chunks, append y
  concurrencia Series=1 son numéricos y no contienen URLs ni identidades.
- Escritura de envelope y actualización de configuración comparten transacción
  tanto en PostgreSQL como en SQLite. El backfill bloquea y reconcilia las tres
  copias legacy —plugin, tuner y configuración Live TV— en una transacción,
  falla cerrado si difieren y deja una referencia canónica. La rotación toma
  locks y re-cifra todas las filas antiguas en una sola transacción; el keyring
  conserva llaves anteriores solo para descifrar. Readiness impide arrancar con
  secretos persistidos sin llave. Al borrar un tuner se hace GC transaccional
  por referencia exacta y se conserva cualquier envelope compartido. El
  arranque ejecuta además un reconciliador global para huérfanos históricos u
  otros paths: PostgreSQL usa aislamiento serializable y locks de envelopes,
  SQLite usa `BEGIN IMMEDIATE`, y cualquier JSON o referencia inválida aborta
  el borrado completo sin impedir que el servidor arranque.
- El vault no elimina por sí solo URLs ya indexadas por versiones antiguas. El
  audit post-reindex ya recorrió el catálogo de staging y obtuvo cero
  `RemoteSourceUrl`, `RemoteMediaProbe.SourceUrl`, probes inválidos y
  `live_tv_channels.stream_url` opacos. El audit complementario de
  journal/logs/argv también quedó limpio. Cualquier import legacy futuro debe
  repetir ambos gates y reimportar el catálogo afectado —no editar filas
  manualmente— antes de abrir ingress.
- Redis queda en **no-go**: no hay cliente, caché ni servicio Redis activo en la
  topología objetivo. Compose conserva solo scaffolding dormido bajo profiles
  para repetir el benchmark o una reevaluación futura; no debe habilitarse con
  la decisión actual. En el benchmark comparable dio 37.665 lecturas/s frente a 37.757 de
  PostgreSQL y añadió memoria. Solo se reabre ante un caso multinodo o una caché
  concreta que supere los gates medidos de `docs/redis-decision.md`.
- Supply chain fija imágenes base por digest, snapshot Debian, FFmpeg, Syft,
  cargo-audit/RustSec y Trivy, usa `cargo --locked`, verifica checksums y conserva
  un registro gobernado de excepciones actualmente vacío. La evidencia de
  `a852c5b` descrita a continuación es la última release completa, no el estado
  del candidato nuevo. En este candidato SQLx 0.9 elimina `rsa` del lock y la
  revisión oficial de FFmpeg incluye un baseline verificable de 16 fixes; queda
  repetir imagen/SBOM/scans antes de sustituir la evidencia histórica. La imagen AArch64 se
  construyó realmente con Podman rootless; Syft generó y verificó los cuatro SBOM
  de imagen/fuente y Trivy/RustSec conservaron evidencia real. El gate queda rojo:
  no basta con que la automatización esté correctamente configurada. El delta
  actual añade FFmpeg al SBOM de imagen como componente CPE, un intento Trivy
  separado para el binario estático, allowlist exacto de decoder, y un smoke
  CI de probe/remux. Trivy 0.70 no demuestra inventario de ese componente
  genérico, por lo que el gate detecta el reporte vacío y falla cerrado hasta
  integrar un matcher FFmpeg validado. La imagen final AArch64 del commit
  `a852c5b81213b444da5ab5d0008defd7628a5934` ya se construyó y analizó: ocupa
  157.058.151 bytes, tiene id
  `922ac2351b0289e307798d2194f93dc5ed135ae43fef39d4c50f5a409b96ef5f`, cero
  encoders y solo el decoder AAC. Su corpus MP4/MKV/MPEG-TS→HLS copy está verde
  como usuario no-root. El SBOM completo verifica; la promoción continúa roja
  por RustSec, Trivy de imagen y la prueba de inventario FFmpeg.

### Evidencia vigente y alcance

El cierre completo vigente del workspace sobre `630a430` registró 695 pruebas
aprobadas, 0 fallidas y 7 ignoradas. La API pasa 353/0/3 usando
`/usr/bin/ffmpeg` para generar fixtures; el resto del workspace pasa 342/0/4.
La superficie PostgreSQL real queda además desglosada en `jellyrin-db`
169/0/4 más doctests, proveedor Xtream 27/27 y migrador 36/36. El objetivo
`202608080120`, con 24 migraciones embebidas, está validado contra PostgreSQL
real y **desplegado** en staging el 2026-08-10 mediante rollout atómico. El
ensure-current de 117 reconcilia el marker sin reconstruir la
proyección ni alterar `xmin`/`completed_at` cuando fuentes y valores ya son
exactos. `check` y `clippy -D warnings` de DB/API/migrador con todos sus targets
y features también terminan limpios. Siguen verdes packaging 46/46,
política supply-chain 46/46, runtime systemd 13/13, unidades systemd 14/14,
performance/recovery 37/37, seguridad 16/16, `git diff --check` y sintaxis Node;
los smokes de systemd, performance y seguridad pasaron sobre el estado vigente.

#### Rollout del esquema 120 en staging (2026-08-10)

Servidor y migrador se instalaron como una unidad desde el mismo build release y
la migración la aplicó únicamente `jellyrin-migrate.service`:

- migración: `202608080119` → `202608080120`, `applied_migrations=1`, 886 ms de
  proceso y 23,3 ms de SQL; migrador `oneshot` con `ExecMainStatus=0`;
- hashes instalados en `/usr/local/bin`, idénticos a `target/release`: servidor
  `f4124471cd525245aca9ada6bab031fccf190c80a144d3937b8aa1a9465997fd`, migrador
  `954ab2743543900af6e0d4e41bc8d2007cfa256370b403d4c50ffd3495ad7810`;
- respaldo pre-120 verificado antes de instalar:
  `/var/backups/jellyrin-postgres/daily/20260810T223557Z`, `sha256sum -c`
  correcto sobre `jellyrin.dump.age`, `database.txt` y `pg_dump-version.txt`
  (PostgreSQL 16.14); binarios anteriores conservados en `/var/backups/jellyrin`
  con sus hashes originales;
- ACL en la base productiva: `jellyrin_runtime` con `SELECT` y nada más sobre
  `media_item_query_filter_summary_values`, `..._coverage` y `..._revisions`;
  `jellyrin_mark_query_filter_summary_dirty`,
  `jellyrin_rebuild_query_filter_summary` y
  `jellyrin_reconcile_query_filter_summary_item` en `SECURITY DEFINER` con
  `search_path=pg_catalog, public, pg_temp`; `PUBLIC` sin `EXECUTE` en las ocho
  funciones de publicación e invalidación;
- reconciliación intacta tras la migración: Movies `1556 = 1556` y Series
  `1989 = 1989`, sin `dirty_at`, con coverage de 39.093/78.380 y
  455.520/911.097;
- health y readiness 200 en `127.0.0.1:8096` y por HTTPS; `NRestarts=0`;
- catálogo Movies: total exacto 39.093 en 77 ms. Filtros `Items/Filters` y
  `Items/Filters2` en 2–8 ms desde el resumen publicado, con 59 géneros en
  Movies y 24 en Series;
- reproducción: VOD compatible resuelto como DirectProxy
  (`SupportsDirectStream`, sin `TranscodingUrl`) y `Range` 206 devolviendo
  1.048.576 bytes exactos de 1.546.982.465 con `video/x-matroska` y **cero**
  procesos FFmpeg; episodio incompatible por HLS con un único job,
  `-threads:v 2`, `-filter_threads 2`, `-filter_complex_threads 2` y
  `CPUQuotaPerSecUSec=1.5s`; un segundo job concurrente quedó en `starting` y
  terminó `failed` sin lanzar proceso, es decir fail-closed;
- Live TV 2961/2965/2958 sirvieron 14–17 MB en 10 s como `video/mp2t` sin
  FFmpeg; 2966 sigue devolviendo 503 y permanece como incidencia de upstream;
- proveedores: `Xtream Codes Provider 1.0.0` y `Jellyrin MAGSTV 0.1.1` activos;
  tuner `xtream-plugin` con 757 canales y credenciales por
  `JellyrinProviderSecretRef`;
- higiene de secretos: 124 líneas de journal sin `api_key`, token de sesión,
  contraseña ni URL; `media_items.path` sin rutas remotas y configuración de
  tuner sin secretos en claro; el `argv` de FFmpeg solo contiene una URL
  loopback, sin query, sin credenciales en ruta y sin token.

Durante los smokes apareció un gate incumplido **anterior a 120**: el listado de
Series respondía 500. `media_item_tv_series_coverage` estaba vacía aunque la
proyección tenía 22.194 series y 455.520 miembros válidos, así que
`tv_series_catalog_page` devolvía `None` y el fallback legacy materializaba
`media_items` completo, superando
`JELLYRIN_DB_API_STATEMENT_TIMEOUT_SECONDS=10`. El mismo error
(`canceling statement due to statement timeout at line 3418`) ya aparece en el
journal a las 20:12 UTC con el binario anterior y esquema 119. Los triggers de
invalidación borran la coverage en cualquier cambio de `media_items` y solo la
reconstruye la sincronización de la carpeta.

#### Página de Series acotada sin coverage publicada

`TvSeriesCatalogPage` reserva `None` para episodios sin `SeriesId`/`SeriesName`
canónicos, no para una coverage caducada, así que ambos drivers ahora recomputan
esa misma página desde las filas vivas cuando falta la coverage. Las tablas de
proyección no se leen: solo su coverage certifica frescura, de modo que la
frontera fail-closed se conserva.

Una sola pasada de agrupación responde lo que certificaría la coverage —
elegibilidad, total exacto y la página pedida— usando `min(...) <> max(...)` para
detectar un segundo valor distinto por grupo sin el orden por grupo que exige
`count(DISTINCT ...)`: esa variante costaba 3,95 s por sí sola. La pasada se
mantiene como un único hash aggregate de una fila por serie con `SET LOCAL
work_mem = '64MB'`, porque los 4 MB por defecto volcaban cada tupla a un merge
externo de 547 MB (2,32 s con volcado frente a 1,17 s y 4,8 MB sin él). Un
`SeriesId` con dos nombres, repartido entre carpetas o fuera de una biblioteca de
TV sigue devolviendo `None` y delegando en la agrupación legacy, que es
exactamente lo que la reconstrucción se niega a proyectar.

Evidencia sobre la base productiva con la coverage todavía sin publicar:

- Series `StartIndex=0` y `StartIndex=40`: 200 con total exacto 22.194, 20 ítems
  por página y orden por nombre correcto, en 4,43 s;
- Movies 39.093 en 0,202 s y `Items/Filters2` de Series en 9,6 ms, sin cambios;
- `jellyrin-db` 170/0/4; API 354/0/3 con `/usr/bin/ffmpeg`; `cargo fmt --check` y
  Clippy estricto de DB, migrador y API limpios;
- binarios reinstalados como conjunto: servidor
  `555ac9b17c11e3f497703c676ee72d889c328543d221cf99b52f1c0f9d89aee2`, migrador
  `c61e2d59a9492ac22b1b93213682fea0927d5092bdd278f53d588d55dd01c889`, con los
  anteriores conservados en `/var/backups/jellyrin`; esquema intacto en
  `202608080120`, `NRestarts=0` y health/readiness local y HTTPS en 200.

#### NextUp acotado sin cambiar su selección

`/Shows/NextUp` fallaba por el mismo motivo pero en su propio camino, y ya lo
hacía a las 20:12 UTC con el binario anterior. Su elección de un episodio por
serie se deriva de `tv_episode_path_info(name, path)`, un parser de rutas en
Rust, así que no puede bajarse a SQL sin duplicar esa semántica: el contrato del
trait ya lo dice explícitamente.

La medición separa el coste real. La consulta de candidatos se planifica y
ejecuta en 556 ms, pero serializar sus 455.520 filas con `media_streams` y
`metadata` cuesta 7,5 s solo en el servidor —ambas son columnas JSONB que hay que
destoastear y decodificar por fila— frente a 1,2 s con el resto de columnas. La
selección nunca lee ninguna de las dos.

Así que los candidatos se piden sin `media_streams` ni `metadata`, la selección
por-serie queda intacta y solo la página retenida se rehidrata con
`media_items_by_ids`, que preserva el orden del llamador. El tipo devuelto llega
con `media_streams` vacío y ambos drivers lo documentan, porque el JSON no
compacto deriva `MediaStreams` y los índices de audio/subtítulo por defecto de
ese campo. Las peticiones con `SeriesId` conservan el camino anterior, que es el
único consumidor de los metadatos de candidatos.

Evidencia sobre la base productiva:

- `/Shows/NextUp?Limit=24`: 200 con total exacto 22.027, 24 ítems, `Type`
  `Episode` y `MediaStreams` hidratado; 8,86 s en frío y 2,89/2,92/2,89 s en
  caliente, frente al 500 por timeout anterior;
- cero 500 en el journal desde el arranque y sin procesos FFmpeg;
- Series 22.194 en 4,30 s, Movies 39.093 en 0,214 s, `Items/Filters2` en 30 ms,
  `Items/Resume`, `Shows/Upcoming` y `LiveTv/Channels` por debajo de 20 ms;
  health y readiness local y HTTPS en 200 con `NRestarts=0`;
- `jellyrin-db` 170/0/4 y API 354/0/3 con `/usr/bin/ffmpeg`; en una de tres
  ejecuciones de la suite de API falló una prueba cuyo nombre no quedó
  registrado y que no reprodujo en las otras dos;
- binarios instalados como conjunto: servidor
  `1f69b351cce46aad7184e76b3a8105698fcc2527f8265733bd507371aec4d637`, migrador
  `e6a22bc40a0709ebc21e34591084b9cd19d7b33d8182d96a4680ef5b293878c5`, con los
  anteriores conservados en `/var/backups/jellyrin`; esquema intacto en
  `202608080120`.

#### Latest exacto y acotado

El camino rápido de Latest ya era acotado pero **aproximaba**: cogía una ventana
de candidatos por `updated_at` y aplicaba después tipos, carpetas excluidas y
ocultar-vistos, así que si esos filtros descartaban más de `ventana - limit` de
los más nuevos devolvía menos ítems de los que existen. Sin `ParentId` no había
camino rápido y materializaba el catálogo completo: 9,02 s sin tipos y 6,47 s con
`IncludeItemTypes=Movie`.

Ahora Latest se responde con `media_item_catalog_page`, la misma maquinaria de
paginación exacta del catálogo, que aplica cada filtro **antes** de su `LIMIT`.
Las carpetas excluidas se resuelven restándolas del ámbito y el ocultar-vistos
viaja como `is_played`. El orden reproduce el comparador
(`updated_at DESC, lower(name) DESC, id DESC`), y cualquier predicado que el
repositorio no exprese **desactiva** el camino en vez de recortar en silencio, de
modo que la aproximación desaparece en lugar de extenderse.

Medido: 16 ítems en 1,58 s sin `ParentId`, 16 películas en 0,26 s con
`ParentId`, y 16 películas en 0,25 s con `IncludeItemTypes=Movie` sin `ParentId`
—esta última forma antes tardaba 6,47 s—. Con miles de filas compartiendo
`updated_at`, «los 16 más nuevos» es ambiguo y el desempate lo decide ahora SQL
en lugar de Rust, así que la mezcla concreta de títulos puede diferir de la
anterior; el conjunto sigue siendo el top-16 exacto bajo un orden total y
coherente con el resto del catálogo.

#### Abrir una serie y una temporada

Con el listado ya operativo apareció el bloqueo siguiente, también anterior a
120: `/Items/{serieId}` respondía 500 tras 22–31 s. Resolver un id de serie o
temporada construía un *snapshot* de **todos** los episodios con sus metadatos.

Ahora hay tres ámbitos, todos subconjuntos estrictos del snapshot completo y con
los mismos predicados: por `SeriesId` persistido (índice parcial existente), por
`SeasonId` persistido (726 ms sin índice: evaluar el predicado no requiere
destoastear, a diferencia de transferir los payloads) y, para los ids sintéticos
derivados del nombre, solo las filas **sin** `SeriesId` canónico, que es su
alcance exacto y está vacío en una biblioteca reconciliada. La clasificación en
Rust sigue decidiendo el resultado, así que un ámbito compuesto solo de extras
continúa devolviendo falso.

Medido tras el arreglo: listado 0,066 s, `/Items/{serie}` 0,051 s,
`/Shows/{serie}/Seasons` 0,004 s, `/Users/../Items/{temporada}` 0,83 s,
`/Shows/{serie}/Episodes` 0,008 s y `/Users/../Items/{episodio}` 0,007 s, todos
200 y sin un solo 500 en el journal.

#### Reproducción: reconciliación puntual y adelantar

Dos defectos distintos rompían la reproducción, ninguno en el catálogo:

1. `PlaybackInfo` sondea el medio remoto y escribe la info resultante, lo que
   dispara `jellyrin_reconcile_query_filter_summary_item`. Esa función rederiva
   los cubos ganadores de toda la carpeta —solo la reagregación escalar cuesta
   1,35 s sobre 455.585 episodios— y superaba el timeout de 10 s de la API. Al
   expirar, la transacción revertía, el siguiente intento volvía a sondear y
   fallaba igual: un bucle permanente que el cliente muestra como carga infinita.
   Las cuatro escrituras que hacen reconciliación puntual pasan al `worker_pool`,
   cuyo presupuesto es de 120 s, así que la escritura confirma. El primer
   `PlaybackInfo` de un ítem recién reimportado tarda 15,8 s y el siguiente
   0,034 s, con el resumen y su coverage intactos: no se degrada la publicación
   de filtros.
2. La clave de dedupe incluye la posición de inicio, así que adelantar siempre
   reclama una sesión nueva mientras el FFmpeg abandonado retiene el único slot
   de admisión hasta su timeout de inactividad de 60 s. La nueva esperaba 15 s y
   moría con `timed out waiting for AudioEncode capacity`. Ahora una sesión nueva
   detiene las otras sesiones en vuelo **del mismo dispositivo**, que es el mismo
   contrato de un stream por cliente. Verificado con dos adelantos consecutivos:
   la anterior queda `stopped`, siempre hay un único proceso y cero fallos de
   capacidad.

Las carátulas que faltaban no eran un defecto propio: la mayoría responde 200 en
~90 ms y unas pocas devuelven `Remote image not found` porque el proveedor no las
tiene.

La base bare-metal de staging ya está desplegada en
`jellyrin.test.kode.live`: PostgreSQL 16.14 y Jellyrin solo escuchan en loopback,
el esquema se aplicó con el rol migrator y el runtime usa su rol separado. El
unit endurecido está activo con `CPUQuota=150%`, `MemoryHigh=1536M`,
`MemoryMax=2G`, `PrivateDevices=true`, familias de direcciones acotadas y
capabilities vacías; `systemd-analyze security` informa exposición 4,2/10
(`OK`). El keyring fuente es `root:root` `0400` dentro de un directorio `0700`
y PID 1 entrega una copia inmutable mediante `LoadCredential`; la copia antigua
legible por el grupo fue retirada después del reinicio correcto. `/health` y
`/readyz` responden 200 localmente y por el proxy TLS, el certificado vence el
2026-11-07, el timer/hook de renovación está activo y el dry-run de Certbot pasó.
Los dos server blocks registran access por path y fijan su error log en `crit`;
`nginx -t` pasó después de recargar. El timer de backup cifrado está activo y un
snapshot real del staging se restauró en una base aislada: 49 tablas, cero
migraciones fallidas, cero constraints inválidos y cleanup completo. La clave y
los snapshots todavía no se han replicado fuera de este host. Esto
acredita la plataforma base, no el E2E con credenciales, clientes y reproducción
reales. `pg_stat_statements` está realmente precargado, su extensión está
instalada en la base `jellyrin` y registra 49 statements. Xtream quedó
configurado y su catálogo contiene 757 canales. El probe HTTP real ya acredita
direct stream y HLS remux del core; los clientes Jellyfin reales siguen fuera
de este cierre.
El candidato vigente del núcleo se desplegó en staging el 2026-08-10 desde
`4513b58`, que incorpora `b774113`, `fd03232`, `1263334`, `e6e2d30`,
`0959ae1`, `585468d`, `506c878`, `33d16bb`, `d82fb29`, `353ddc1` y `954369c`.
El servidor tiene SHA-256
`cef61151d98b24a1875754aefd0fde74e6e25ae4a0ee184271fe5429a6069c2a`; el
esquema PostgreSQL está en 119 y el migrador embebe 23 migraciones. El migrador
desplegado tiene SHA-256
`2e161e19b7f4e614371e48e7ea2b444678817ab7576b8a4a348938bd0738f5e2`.
FFmpeg/ffprobe mínimos en `/usr/local/bin` son
`8.2-dev-git-1e0279143db9`, con hashes
`26db77886b8575201bdb24e1a5f60b26f7b1f7d42ccdf31bb4260dc1ce76ab5d` y
`66bdb10657041d982efea72210f1833b5e2c594faf75f0a79d43a4c3b5f9e4fe`.
Los binarios inmediatamente anteriores de servidor y migrador se conservan en
`/var/backups/jellyrin/*-pre-b774113-20260810T065000Z`. El shutdown previo no tuvo timeout ni
hijos residuales. El rollout 117 se protegió con el dump root-only
`postgres-pre-schema-117-20260810T034557Z.dump` (209.290.844 bytes, SHA-256
`c7833346caec5462afcc5dba77bca2c762b46fc96bce1ca9f82179ce36bb704e`).
La migración productiva 116→117 tardó 63,346 s y consumió 14,507 s de CPU; el
segundo `schema` quedó `schema_current` en 398 ms. El rollout 116 anterior usó
`postgres-pre-schema-116-20260810T012300Z.dump` (SHA-256
`a62143ac5822bdf811b5a965b278359a34d5cccf1b6ca10b46fd6dcccb584439`):
la migración proyectó 455.520 miembros y 22.194 series en 25,1 s, y el arranque
validó explícitamente las capacidades FFmpeg en modo
`RemuxOnly`, y `/health`/`/readyz` local y HTTPS quedaron verdes con cero
reinicios tras el arranque final. Un primer binario de servidor, enlazado antes
de optimizar el SQL 116, rechazó correctamente el checksum aplicado y no abrió
el puerto; se recompiló contra el mismo artefacto embebido del migrador antes de
reanudar servicio. Este es un rollout de staging para E2E; los gates de vulnerabilidades
siguen rojos y bloquean promoción. El rollout 118 se protegió con
`postgres-pre-schema-118-20260810T064600Z.dump` (144.247.894 bytes, modo 0600,
SHA-256 `e7cd96beae4c52c94ac9cb42fe95bf4a504cefcff6c7979cf0ebe2e6b954b16d`).
La migración productiva 117→118 tardó 11,528 s; el servidor arrancó con salud y
readiness local/HTTPS verdes, cero warnings y cero reinicios.
El rollout 119 se protegió con
`postgres-pre-schema-119-20260810T074744Z.dump` (144.091.124 bytes, modo 0600,
SHA-256 `b1d6d248890e6848f8555291fbc37221bb096de327a809871f7fe06e33262c00`)
y copias de los binarios 118 bajo
`/var/backups/jellyrin/*-pre-4513b58-20260810T074744Z`. La migración productiva
118→119 tardó 438 ms; el rerun idempotente quedó en 386 ms. Salud/readiness
locales y TLS quedaron verdes, `NRestarts=0`, 2/2 revisiones limpias y cero
coverage stale. El probe Xtream real y la carga HTTPS 80/8 posteriores no
iniciaron FFmpeg.

La migración 106 se ejecutó realmente en PostgreSQL 16.14 y aceptó filas legacy
y opacas válidas; sus tests SQLite rechazan mixed/neither y preservan filas en
preflight. El migrador pasa 36/36 pruebas locales —32 de librería y 4 de
binario— y clippy estricto tras incorporar `provider_secrets`, el tipo `Bytes`
y clasificar `catalog_sync_runs` como historial operacional omitido. El fixture
incluye NUL, `0xff` y bytes no UTF-8, con igualdad exacta de nonce/ciphertext;
ese round-trip también pasó contra PostgreSQL real. Xtream pasa 27/27
post-XOR/ImageUrl y su clippy estricto.

El repositorio MAGSTV externo integra `origin/main` `2700d7f` mediante el merge
`43551fe`; sobre esa base, la adaptación local `ExternalProcess` queda en
`8ce47b4` y la versión 0.1.1 en `9596f1c`. Contra SDK/RPC del core local pasa 91
pruebas, 0 fallidas y 4 ignoradas; además pasan su clippy estricto, `cargo fmt
--check` y `git diff --check`. Las cuatro ignoradas requieren una cuenta real.
El pin público sigue apuntando a la revisión anterior del core y solo puede
actualizarse después de publicar la revisión compatible de Jellyrin.
Para no bloquear el E2E se construyó y validó el ZIP AArch64 0.1.1 contra el
árbol local. Su SHA-256 es
`00cb1db58101c3b4af3041431c52bef5296cb650a552b64bbf9a64dbbc01a92f`.
La UI fue corregida para solicitar únicamente las credenciales de cuenta; no
presenta settings operativos como si fueran secretos configurables. El ZIP
0.1.1 y su índice están en `/srv/jellyrin/plugin-repository`. Tras un backup
PostgreSQL completo y otro del plugin 0.1.0, staging refrescó el repositorio,
instaló y activó `Jellyrin MAGSTV` 0.1.1. El endpoint admin de configuración
responde 200 con el objeto operativo y sin campos `Username`/`Password`; no
existe todavía ningún tuner MAGSTV y la salud queda `Degraded` por esos
prerrequisitos ausentes, no por un crash del host. Egress, secretos
operativos del proveedor y el E2E real siguen pendientes. Este artefacto local
no debe publicarse como release ni presentarse como rollout completado.

Los rollbacks previos a esa actualización son
`postgres-pre-magstv-0.1.1-20260810T000255Z.dump` y
`magstv-plugin-pre-0.1.1-20260810T000255Z.tar.gz` dentro de
`/var/backups/jellyrin`.

El audit post-import counts-only quedó limpio el 2026-08-10: PostgreSQL recorrió
494.613 items en 19.587 ms y devolvió 0/0/0/0 para URL remota, URL de probe,
probe inválido y stream opaco. El primer intento reveló que combinar los tres
scans JSON excedía el timeout por sentencia de 10 s; `4222b6b` los separa sin
elevarlo y pasa 36/36 más Clippy estricto. El audit de runtime desde el rollout
escaneó cuatro fuentes completas en 315 ms y devolvió cero en las cinco clases
de fuga. Los reportes root-only están en
`/var/backups/jellyrin/source-hygiene-20260810T004600Z.json` y
`/var/backups/jellyrin/runtime-hygiene-20260810T004500Z.json`.

Este bloque refleja el árbol de trabajo, no una release publicada. Cada elemento
se marcará completo solo después de su validación y rollout correspondiente.

### Matriz de evidencia y alcance

| Área | Código local | Evidencia ejecutada | Fuera de este cierre |
| --- | --- | --- | --- |
| Drivers y runtime PostgreSQL | Costura de selección; PG único productivo, SQLite real para test/migración y MySQL solo reservado; sin `AnyPool`, fallback ni SQLx en API; telemetría real de hot paths por pool | DB 169/0/4 más doctests y staging durable sobre PostgreSQL real; migrador 36/36; esquema 119 validado en clon real y aplicado con backup/rollback preparado | E2E restante antes de otro backend |
| FFmpeg/proxy/shutdown | Direct/remux/encode parcial, copy-first, intención tipada y clasificador fail-closed, cupos/process groups/cuota/watchdogs; FFmpeg `8.2-dev-git-1e0279143db9` mínimo sin encoders y solo decoder AAC; redirects opacos revalidados por salto | Corpus aislado verde en imagen ARM64 exacta de HEAD; VOD real DirectProxy 206/65.536 bytes con 0 FFmpeg; Live TV real leyó 112.827 bytes directo con 0 FFmpeg y HLS produjo 1.702.152 bytes como remux, sin fallback, ~15,3 MiB RSS, PID reap y leases 0→0 | Matriz de clientes reales, medidas sostenidas/concurrentes del host y límite físico del volumen; repetir AMD64 |
| MAGSTV | Referencias opacas, JIT, grant core persist-first, proceso one-shot, lock R/W, detector y esquema seguro implementados; UI corregida a credenciales-only; `origin/main` `2700d7f` integrado por `43551fe`, adaptación `ExternalProcess` `8ce47b4` y versión 0.1.1 `9596f1c` | 91/0/4 ignoradas contra SDK/RPC local; ZIP AArch64 0.1.1 validado e instalado/activo en staging tras backups; configuración admin 200 sin credenciales; salud `Degraded` esperada sin tuner/egress; clave de referencia root-only generada | Pin público aún viejo; perfil WireGuard MX, metadatos/secretos legítimos restantes, cuenta/tuner, E2E real y publicación pendientes |
| Xtream integrado y vault | Referencias JIT, relay loopback, XOR Live TV, AEAD; VOD/Series por streaming acotado a staging durable, fallback Series por categoría, publicación conjunta y `0 = todo`; probe media+metadata agrupado; métricas counts-only y límites efectivos | Xtream 27/27; sync real completo en 4.969 s con 39.093 películas, 455.520 episodios, 3 series omitidas, 0 duplicadas, pico 158.728.192 bytes y 0 stages residuales; Live TV direct/remux real verde; audits DB/logs/argv 0 findings | Repetición periódica del audit y matriz de clientes reales |
| Catálogo general | Pushdown SQL paginado con total exacto, playback join y ParentId; Series usa una proyección durable/atómica por driver, página claves canónicas y conserva fallback legacy fail-closed; filtros normalizan `Series`/`Movie,Series` a episodios persistidos | API 354/0/3, DB 169/0/4 y Clippy estricto sobre el lote; migrador baseline 36/36; PostgreSQL real: 455.520 episodios/22.194 series, rebuild 25,1 s; 80 páginas a concurrencia 8, 0 fallos, p50 448 ms, p95 669 ms, p99 895 ms y total exacto. Tras el rollout 120, Movies devuelve 39.093 en 77 ms y Series 22.194 exactos en 4,30 s mediante la página acotada en vivo que sustituye al fallback sin límite cuando la coverage no está publicada; NextUp 22.027 exactos en 2,89 s pidiendo candidatos sin JSONB e hidratando solo la página | Acotar `Items/Latest`; E2E visual/reproducción |
| Facetas y filtros | Proyección item-level 117, resumen exacto 118, revisiones/CAS 119 y frontera de publicación 120 por carpeta/tipo; ganador determinista, coverage revisionada e invalidación fail-closed; runtime sin DML directo ni bypass GUC | 494.613 items/989.226 contribuciones → 96 filas; 119 en 438 ms y 120 en 886 ms sobre la base productiva. PostgreSQL 16 aislado valida ACL, rebuild/punto y ataques con GUC/sombras temporales; ACL de solo lectura y `SECURITY DEFINER` reverificados en staging tras el rollout; API 354/0/3 y DB efectiva 169/0/4 | Scope padre+hijos/múltiple, coalescer de grandes lotes y E2E cliente real |
| Redis | **No-go** y apagado | Benchmark reproducible: sin mejora frente a PG y con memoria adicional | Solo reabrir por caso multinodo o caché medida concreta |
| Supply chain | Pins, SBOM/scanners/excepciones gobernadas; runtime distroless sin shell/package manager; SQLx 0.9 sin `rsa`; FFmpeg por commit con 16 fixes oficiales verificados y NVD fail-closed; Jellyfin Web endurecido | Sobre HEAD `630a430`: supply-chain 46/46, packaging 47/47, security-hardening 16/16, systemd 14/14, performance/recovery 37/37; imagen Docker AArch64 nativa `e561d9fe178a` de 88.538.826 bytes con healthcheck de imagen, corpus y runtime smokes verdes, Compose real hasta esquema 117, SBOM verificado y RustSec/Trivy/NVD `passed=true` | Repetir todo en AMD64 nativo; después firma/provenance y pull por digest |
| Staging bare-metal | PostgreSQL/runtime separados, loopback, TLS, renovación, logs proxy sin query, keyring por `LoadCredential`; FFmpeg software habilitado con un job, dos threads, niceness 10 y techo físico de 150% de CPU | Núcleo `c89ccd8`; esquema 120 desplegado el 2026-08-10 a las 22:46 UTC en 886 ms; health/readiness local/HTTPS verdes y 0 reinicios; 757 canales, 39.093 películas, 22.194 series y 455.520 episodios. VOD compatible directo 206 con `Range` exacto y sin FFmpeg; HLS incompatible con un job, `-threads:v 2` y 150% de CPU, y segundo job concurrente rechazado fail-closed. Live TV 2958/2961/2965 verde y 2966 sigue caído en upstream (503); MAGSTV 0.1.1 activo. Listado de Series 200 con total exacto 22.194 en 4,30 s por la página acotada en vivo, con la coverage aún sin publicar; `/Shows/NextUp` 200 con 22.027 exactos en 2,89 s y streams hidratados; Latest exacto por `media_item_catalog_page`; abrir serie/temporada/episodio en milisegundos; `PlaybackInfo` y adelantar sin bucle de fallo | Abaratar la reconciliación puntual para que el primer `PlaybackInfo` no cueste 15,8 s; E2E visual autenticado; worker externo/hardware para 4K; resolver egress/secretos operativos y ejecutar E2E MAGSTV; backups off-host |

### Trabajo restante y gates de salida

El cierre se divide expresamente para no confundir código terminado con un
rollout probado:

1. **Cierre dirigido de GenreIds y Upcoming — completado:** API 353/0/3, DB
   169/0/4 más doctests y migrador 36/36 sobre PostgreSQL 17.10 real; check y
   clippy estrictos, formato, packaging, supply-chain e higiene runtime verdes.
   El workspace completo vigente pasa 695/0/7. Este cierre no sustituye el E2E
   con proveedores y clientes reales.
2. **Cierre y rollout JIT para Xtream integrado:** el código ya persiste
   `RemoteSourceRef={Version,Provider,TunerId,Kind,RemoteId,Extension}`, resuelve
   la URL justo antes de sync/proxy/probe y usa la revisión opaca del secreto.
   El arranque valida llaves, rota envelopes y hace dual-read/backfill
   transaccional de plugin/tuner/livetv hacia una referencia única. La migración
   de invariante Live TV y las migraciones de catálogo hasta el esquema 117 ya
   están aplicadas en staging. La configuración real está dada de alta y el
   catálogo indexa 757 canales. El bloqueo de más de 100.000 VOD, el catálogo
   Series global >64 MiB y el listado posterior de Series están resueltos y
   desplegados: con `SeriesLimit=0`, el sync real publicó 39.093 películas y
   455.520 episodios agrupados en 22.194 series; omitió 3 detalles remotos
   persistentemente transitorios sin invalidar el resto de la generación.
   DirectProxy VOD real ya conserva H.264/AAC sin FFmpeg y sirve rangos a través
   de la redirección CDN validada. Live TV real expone direct y HLS para la
   referencia opaca: el directo no crea FFmpeg y el HLS usa stream-copy remux
   sin fallback, con stop/reap y leases restituidos. Faltan clientes reales y
   carga concurrente/sostenida. Los audits counts-only posteriores ya dieron
   cero `RemoteSourceUrl`, `RemoteMediaProbe.SourceUrl`, probes inválidos,
   `live_tv_channels.stream_url` opacos y fugas en DB/logs/argv. Deben repetirse
   tras cualquier import legacy. `direct_source` no reconstruible se rechaza: no se persiste
   como excepción por item.
3. **Cierre MAGSTV entre repositorios:** el parche externo para exigir `Network`
   + `ProviderSecrets`, validar plugin/tuner/acción/id/revisión, transferir el
   grant a un secreto zeroizing y retirar `MAGSTV_SECRET_*` ya está aplicado al
   árbol MAGSTV. `origin/main` `2700d7f` se integró por el merge `43551fe`; la
   adaptación `ExternalProcess` local es `8ce47b4` y la versión 0.1.1 queda en
   `9596f1c`. Contra SDK/RPC local pasa 91 aprobadas, 0 fallidas y 4 ignoradas,
   con clippy/fmt/diff verdes. El ZIP AArch64 0.1.1 está validado y el repositorio
   staging preparado. El repositorio se refrescó y 0.1.1 quedó instalado,
   habilitado y con configuración admin accesible; se conservaron backups de
   PostgreSQL y 0.1.0. Repetir
   la matriz contra el core publicado compatible y, después de publicar
   Jellyrin, fijar el commit SDK/RPC exacto en el plugin; el pin público actual
   sigue viejo. La
   configuración y el login real se reservan para el E2E posterior. El plugin
   ya incorpora una página administrada corregida para aceptar únicamente
   credenciales de cuenta; egress, secretos operativos y E2E real siguen siendo
   gates separados. El core enumera y
   sirve únicamente HTML declarado, regular, confinado, acotado y admin-only.
   Como respaldo, `qa/magstv-configure-jellyrin.js` ofrece validación offline,
   preflight sin mutaciones e import autenticado sin imprimir secretos.
   Ambos caminos escriben por `/LiveTv/TunerHosts`, donde Jellyrin cifra usuario
   y contraseña antes del RPC.
4. **Escala PostgreSQL:** `/Items` y `/Users/{id}/Items` ya hacen recuento
   exacto más `LIMIT/OFFSET`, playback join y filtros SQL para el subconjunto compatible,
   incluido `ParentId` de carpeta virtual. Counts, colecciones de metadata y los
   dos endpoints Filters agregan el catálogo completo compatible sin depender
   del cap de página y conservan fallback exacto para shapes complejos. La
   importación Xtream usa chunks y staging porque su API no garantiza una
   paginación global interoperable; la paginación se aplica después contra la
   base local. `Series` es una vista sintética agrupada desde episodios y sus
   filtros normalizan también las consultas mixtas `Movie,Series`.
   La
   proyección facet/selectores/fechas 108/109/110/111/112 evita scans y backfills repetidos. Las peticiones de listado sin `Limit`, filtros
   metadata no modelados, Series/Season, sugerencias, resume sin límite o con
   filtros/orden complejos y episodios especiales todavía pueden ejecutar
   `fetch_all`, filtrar/ordenar después en Rust o hacer N+1. Resume simple con
   límite ya ejecuta policy, total y página dentro del adaptador SQL. El cierre
   requiere caps duros o nuevos contratos SQL para
   cada fallback y columnas/relaciones normalizadas donde JSONB no escale.
   El harness sintético ya mide `EXPLAIN (ANALYZE, BUFFERS)` y p95 a
   10k/100k/500k; el rerun PG17.10 no justificó otro índice sólo para Movie
   page (0,954x/1,035x/1,127x), mientras las proyecciones Upcoming y
   Person/Studio/Tag sí redujeron candidatos y mejoraron sus consultas. La
   evidencia y su digest quedan fijados más abajo. Falta repetir
   con distribución y handlers E2E representativos. El aislamiento de
   pools ya se prueba saturando realmente API y worker en ambos sentidos, y un
   runner local compara p50/p95/p99 y throughput de API con el worker saturado.
   `/Shows/Upcoming` común ya no carga el dominio TV completo ni hace una
   segunda hidratación: ambos drivers transmiten las filas visibles TV con
   metadata inline y solo retienen episodios con fecha futura. La clasificación,
   precedencia de claves y parser RFC3339 son compartidos, y los shapes con
   filtros no equivalentes conservan el fallback. La ruta común consulta primero
   `media_item_upcoming_dates` y fuerza búsquedas puntuales por PK, conservando
   la comprobación Rust como guard de equivalencia. A 10k/100k/500k bajó de
   3.300/33.000/165.000 filas TV a 34/334/1.667 futuras y mejoró el p95 SQL
   2,467x/3,665x/2,565x; los planes usan el range index y búsquedas por PK.
   Person/Studio/Tag combinados usan también una intersección conducida por su
   índice y un `LATERAL ... OFFSET 0` como barrera medida: a
   10k/100k/500k transportan 24/247/1.234 filas visibles en vez de
   9.900/99.000/495.000, y el p95 SQL mejora 2,571→1,891 ms (1,360x),
   25,742→16,848 ms (1,528x) y 142,846→109,361 ms (1,306x). Los planes
   ejecutan búsquedas puntuales por PK en lugar de conservar un scan completo.
   Esto
   no bloquea un rollout exclusivamente MAGSTV, que ya usa `live_tv_*`, pero sí
   bloquea afirmar esa escala para bibliotecas generales.
5. **Supply chain real — runtime distroless AArch64 exacto validado:** Rust
   1.94/SQLx 0.9 eliminan `rsa` del lock sin excepción. La etapa FFmpeg AArch64
   del candidato ya compila desde la revisión oficial `1e027914...`; verificó
   el archivo fuente y los 16 patches oficiales por SHA-256, y NVD devolvió 16
   HIGH/CRITICAL, todos mapeados. La imagen exacta `8026d7f` pesa 157.937.350
   bytes, id `44df144e...bcec`; corpus y SBOM pasan. RustSec=0 y NVD-FFmpeg=0,
   pero Trivy OS bloqueó 13 CVE únicas/22 ocurrencias. La evidencia histórica está en
   `/home/ubuntu/plans/generated/supply-chain-arm64-8026d7f` (SHA del manifiesto
   `61a5ee9e...98dfc`) y `vulnerability-arm64-8026d7f` (`22073e6e...3d00`).
   La remediación sustituye la etapa final Debian por el manifest inmutable
   `gcr.io/distroless/cc-debian13:nonroot@sha256:d97bc0a...b23775`, conservando
   Debian solo como helper de build. El candidato final ejecuta FFmpeg y
   ffprobe dentro de distroless durante el build, pasa el corpus MP4/MKV/MPEG-TS
   como no-root y genera un SBOM verificable de 13 paquetes OS más Jellyrin.
   Trivy 0.70.0 detecta Debian 13.6 y devuelve 0 HIGH/CRITICAL. La imagen exacta
   de `6a15aec579b8992373c72ae9f57b3503ef3751dd` tiene id
   `3f1f0b7f...0b84`, pesa 87.663.302 bytes y conserva la revisión OCI completa.
   RustSec, Trivy y NVD-FFmpeg terminan todos con exit code cero. Los bundles
   están en `/home/ubuntu/plans/generated/supply-chain-arm64-6a15aec`
   (`SHA256SUMS` `4b130aca...f37dc`) y
   `/home/ubuntu/plans/generated/vulnerability-arm64-6a15aec`
   (`SHA256SUMS` `feb500de...8771`). Falta repetir todo en AMD64. Como evidencia
   histórica adicional,
   Podman rootless
   construyó la imagen AArch64 exacta de `a852c5b81213b444da5ab5d0008defd7628a5934`,
   con revisión OCI completa, sin `curl`, FFmpeg 8.1.2 mínimo fijado y 157.058.151
   bytes. Syft generó SPDX/CycloneDX para imagen, fuente y FFmpeg; todos los
   `SHA256SUMS` verifican. El bundle válido está en
   `/home/ubuntu/plans/generated/supply-chain-arm64-a852c5b81213-complete`
   (`SHA256SUMS` `67cbf7294e1d142150d4e65374e10850895f86ae1fa19da10591451445d019c5`).
   RustSec bloquea por `rsa 0.9.10`/RUSTSEC-2023-0071 y Trivy 0.70.0 bloquea 13
   CVE únicas HIGH/CRITICAL en paquetes Debian (22 ocurrencias: 5 críticas y 17
   altas, sobre 15 paquetes). Trivy-FFmpeg termina además con estado 98 porque
   no prueba que inventariase el CPE. La evidencia completa está en
   `/home/ubuntu/plans/generated/vulnerability-arm64-a852c5b81213`
   (`SHA256SUMS` `edf1470905f9dc7840e0866bf63f02610f4e728f34f64e5ff229e0923d28cdc3`).
   Los avisos `unsound` de
   `anyhow 1.0.102` y `event-listener 5.4.1` se remediaron en `Cargo.lock` con
   1.0.103 y 5.4.2. `rsa` solo llega al lock mediante el backend opcional
   `sqlx-mysql`: `cargo tree -i rsa` queda vacío tanto para producción como para
   workspace `--all-features --target all`, y ninguna feature `mysql` aparece
   en los manifests o en el árbol de features. No existe versión parcheada
   declarada por el advisory. Las opciones requieren revisión explícita: dejar
   el gate rojo; aprobar temporalmente una excepción gobernada y acotada a
   `crate:rsa@0.9.10` por código no alcanzable; o analizar cómo excluir el
   backend opcional del lock/cambiar la composición SQLx sin subir SQLx a
   ciegas. Para Trivy se debe contrastar cada finding con Debian Security
   Tracker: por ejemplo, Debian marca CVE-2023-45853 de zlib como no aplicable
   al binario Bookworm porque MiniZip no se construye, mientras FFmpeg
   CVE-2026-58049 sigue sin fix incluso en Trixie; no se aceptarán findings en
   bloque ni se cambiará de distribución a ciegas. Después hay que actualizar o
   minimizar el runtime. La siguiente intervención con impacto material es un
   build reproducible y mínimo de FFmpeg orientado a direct/remux, porque el
   paquete Debian completo introduce gran parte de la superficie multimedia y
   gráfica que Jellyrin no necesita. Ese build 8.1.2 ya está implementado y su
   corpus aislado pasa; además se corrigió el verde falso del binario estático:
   se añade CPE y el gate falla si Trivy no demuestra que lo inventarió. Trivy
   0.70 todavía no lo demuestra, por lo que falta integrar un matcher válido.
   Después se debe construir/analizar AMD64 y solo con
   gates verdes firmar el digest y adjuntar provenance. El QA 46/46 solo
   acredita política y pins, no sustituye estos resultados reales. El Jellyfin
   Web endurecido se construyó localmente y su gate Playwright aislado pasó
   1/1 sobre PostgreSQL 16 real: wizard y login, foto servida por descarga,
   inicialización/OSD/autoplay del slideshow y CBZ de tres páginas con worker,
   navegación, RTL y vista doble. El recorrido detectó además un alias SQL
   reservado (`session_user`) en el broadcast de sesiones; se corrigió y el
   test PG de sesiones cubre ahora todos los reads que consume ese broadcast.
6. **Staging — plataforma base desplegada; E2E HTTP Xtream parcial:**
   `jellyrin.test.kode.live` ya ejecuta el binario release contra PostgreSQL
   local mediante roles separados, backend loopback, proxy TLS, keyring por
   credencial systemd y límites software-only. Migraciones, health/ready,
   renovación de certificado y restore drill están verdes; `pg_stat_statements`
   está precargado, instalado en la base `jellyrin` y registra 49 statements.
   Desde una sesión autenticada ya se configuró Xtream y se
   indexaron 757 canales. El primer `SyncXtreamMedia` real detectó un bloqueo
   adicional: `get_vod_streams` devolvió más de 100.000 filas y el importador
   aplicó ese límite al catálogo bruto antes de filtrar las 60 categorías VOD.
   La publicación conjunta se abortó correctamente y no alteró el catálogo
   visible. La corrección de escala de 13.6 y la migración 113 se desplegaron
   desde `ff9412e`; `SeriesLimit` pasó de 250 a 0 sin alterar el resto de la
   configuración. Una ejecución intermedia de alcance limitado publicó 39.093
   películas y 1.974 episodios en 186 segundos; no era todavía el catálogo
   Series completo. Tras corregir los límites cero, el aislamiento de entradas
   malformadas y los reintentos acotados de detalles transitorios, el sync
   completo publicó atómicamente 39.093 películas y 455.520 episodios de 22.194
   series en 4.969 segundos. Omitió 3 series con fallo remoto persistente, no
   deduplicó ninguna y dejó 0 stages residuales. El pico de memoria del servicio
   durante la ingestión fue 158.728.192 bytes, con 0 reinicios y
   health/readiness verdes. Las migraciones 114/115 trasladaron después el
   agrupado y la página de Series a SQL; la 116 eliminó el reagrupar JSON por
   petición mediante una proyección transaccional. La carga real de 80 páginas
   a concurrencia 8 mantiene el total exacto, no falla y reduce p95 de 6.169 a
   669 ms. La matriz sintética 10k/100k/500k ya se ejecutó. DirectProxy VOD real
   ya respondió 206 a una lectura acotada de
   65.536 bytes a través de la redirección CDN, sin lanzar FFmpeg y con unos
   40,7 ms de CPU del servicio. El probe Live TV posterior recorrió como máximo
   25 filas y encontró una fuente funcional en el segundo intento: directo
   200/112.827 bytes con cero FFmpeg; HLS 200/1.702.152 bytes como `remux`, sin
   fallback, ~15,3 MiB RSS y CPU instantánea 0%; stop dejó PID reap, leases y
   FFmpeg en su baseline cero. MAGSTV 0.1.1 está instalado y activo, pero sin
   tuner: faltan sus secretos operativos/egress y una cuenta controlada, carga
   concurrente, matriz Jellyfin Web/TV/DLNA y reproducción sostenida. Ninguna
   credencial real se incorpora a fixtures o logs. El backend queda en
   `127.0.0.1`; la plantilla Nginx registra método + `$uri`, nunca query,
   referrer, `$request` ni `$request_uri`, para que `api_key` no llegue al
   access log. Ambos server blocks ya fijan el error log en `crit`, porque los
   mensajes de error de Nginx pueden incluir la request completa. El gate E2E
   pendiente no impide usar el formulario inicial, pero sí declarar
   proveedores/clientes reales y rollout completados.
7. **Cutover y publicación:** generar backup verificado, aplicar esquema con el
   rol migrator y arrancar con el rol runtime. La migración SQLite→PostgreSQL y
   sus digests solo son obligatorios si se entrega un snapshot legacy real; el
   staging actual nació en PostgreSQL y reindexa catálogos externos. Después se
   publican commits/releases separados
   y se actualiza el pin del repositorio MAGSTV al commit público de Jellyrin;
   los commits locales del core, incluidos `506c878` y `585468d`, aún no se han
   enviado a `origin`.

Redis no está en esta lista: el resultado medido fue **no-go** para este nodo y
permanece apagado. Sólo vuelve a evaluación si aparece coordinación multinodo o
una caché concreta que supere los gates de
[`redis-decision.md`](redis-decision.md).

## 1. Contexto y objetivo

Jellyrin se utilizará principalmente como índice y pasarela de servicios externos
(Xtream Live TV, VOD y series), no como servidor de una biblioteca local. El
plan optimiza conjuntamente persistencia, sincronización del catálogo,
consultas, caché, reproducción y uso de CPU. No basta con cambiar un driver ni
con añadir flags aislados a FFmpeg.

Las decisiones de arquitectura son:

1. PostgreSQL será la fuente de verdad y el único backend habilitado en
   producción en esta entrega.
2. La selección se centraliza en `DatabaseDriver`/`DatabaseManager`, al estilo
   de un manager de framework, sin esconder el dialecto. PostgreSQL, SQLite y
   MySQL son selectores reconocidos; PostgreSQL y SQLite tienen adaptadores
   reales con alcances distintos, mientras MySQL es solo una reserva explícita.
   Hoy solo PostgreSQL es `production-ready`.
3. SQLite tiene adaptador y selector canónico `sqlite`, con `sqlite-legacy`
   como alias compatible. Permanece detrás del feature `sqlite` para tests y
   migración histórica; el manager productivo lo rechaza de forma explícita y
   jamás lo usa como fallback si PostgreSQL falla.
4. MySQL queda reconocido como selector reservado. Para habilitarlo deberá
   aportar adaptador, SQL, migraciones y suite de conformidad nativos; no se
   introducirá `AnyPool` ni un mínimo común de consultas.
5. Redis no se integra en la topología actual tras su resultado no-go. Solo se
   reconsiderará para una caché o coordinación multinodo medida; nunca será
   fuente de verdad ni propietario de un FFmpeg.
6. Los catálogos externos son reconstruibles; usuarios, configuración,
   credenciales, progreso y listas son datos irremplazables y se migrarán con
   verificación estricta.
7. En reproducción se elegirá la ruta más barata compatible con cada cliente:

   1. **Direct stream mediante proxy HTTP**: sin FFmpeg.
   2. **Remux/transmux**: FFmpeg segmenta o cambia el contenedor usando stream copy.
   3. **Transcodificación parcial**: copiar vídeo y convertir solo audio, o viceversa.
   4. **Transcodificación completa**: último recurso.

Las métricas principales serán la latencia de catálogo, el tiempo y coste de
sincronización, y el porcentaje de reproducciones que evitan recodificar vídeo.
Los límites de CPU son una protección secundaria; por sí solos no reducen el
trabajo necesario y pueden provocar buffering.

La migración de base de datos y la optimización FFmpeg se entregarán en cambios
separados y reversibles. Pueden avanzar en paralelo después de disponer de
métricas, pero no se desplegarán simultáneamente en producción: así se puede
atribuir cualquier regresión y hacer rollback de una sola variable.

## 2. Diagnóstico de la línea base original

Esta sección conserva el diagnóstico previo a la implementación para explicar
por qué se tomaron las decisiones. El estado vigente y los pendientes exactos
son los del bloque inicial; por tanto, los presentes de esta sección no deben
interpretarse como descripción del árbol de trabajo actual.

### 2.1 Construcción original de FFmpeg

En la línea base, toda la salida HLS pasaba por `build_hls_ffmpeg_command` en
`crates/jellyrin-core/src/lib.rs` y presentaba estas limitaciones:

- El vídeo siempre se convierte con
  `libx264 -preset veryfast -profile:v main -pix_fmt yuv420p`.
- El audio siempre se convierte a AAC estéreo.
- No existen modos `copy`, encoder configurable ni aceleración hardware.
- `HlsTranscodeRequest` tiene campos de bitrate y resolución, pero las rutas
  principales de reproducción no los rellenan.
- FFmpeg no recibe `-readrate`, por lo que un VOD remoto se ingiere y codifica
  tan rápido como permitan red y CPU, aunque el cliente reproduzca a velocidad
  normal.
- `-hls_list_size 0` conserva todas las entradas de la playlist.

Los procesos se lanzan desde `jellyrin-transcode`, pero también hay ejecuciones
directas de `tokio::process::Command` para segmentos bajo demanda, subtítulos,
miniaturas y trickplay. Un límite aplicado únicamente a
`spawn_transcode_process` no cubriría todos los consumidores de FFmpeg.

### 2.2 Decisión de reproducción remota

En `playback_info_response`, todo elemento Xtream remoto quedaba excluido de
direct play y direct stream aunque `ffprobe` hubiera confirmado que sus códecs
eran compatibles. Como consecuencia, VOD y series remotos iban a HLS con
recodificación.

Live TV ya disponía de un proxy que compartía una conexión upstream entre varios
consumidores del mismo canal. Sin embargo, `live_tv_playback_info_response`
anunciaba `SupportsDirectStream=false`, eliminaba `DirectStreamUrl` y obligaba
al cliente a utilizar HLS transcodificado.

Los endpoints generales de direct stream existían, pero `stream_media_item`
solo abría archivos locales. Un item virtual `xtream://...` todavía no podía
usar esa ruta para entregar `RemoteSourceUrl` por proxy.

### 2.3 Compatibilidad original del cliente

Jellyrin interpretaba `DirectPlayProfiles`, pero todavía no usaba de forma
completa:

- `TranscodingProfiles`.
- `CodecProfiles` y sus condiciones de perfil, nivel, bit depth, resolución o
  canales.
- `MaxStreamingBitrate` y límites equivalentes.
- Restricciones de ancho y alto solicitadas por el cliente.

Por tanto, la decisión original era demasiado binaria y no podía escoger de
forma segura entre direct stream, remux, transcodificación parcial y completa.

### 2.4 Concurrencia y ciclo de vida

- No había límite global de recodificaciones simultáneas.
- No había cupos separados para remux, vídeo software, miniaturas o trickplay.
- Los segmentos generados bajo demanda podían iniciar otro FFmpeg mientras la
  transcodificación continua seguía activa.
- Si un cliente desaparecía sin enviar el cierre de sesión, el proceso VOD podía
  continuar hasta terminar el archivo.
- Las sesiones completadas podían conservar HLS durante 24 horas.
- Live HLS utilizaba playlist `event` y mantenía todos los segmentos; una sesión
  larga podía crecer indefinidamente.
- El registro de deduplicación evitaba algunas sesiones idénticas, pero no
  actuaba como planificador de recursos.

### 2.5 Hardware del servidor actual

El host tiene cuatro núcleos ARM Neoverse-N1. `/dev/dri/renderD128` pertenece a
`virtio-pci`; no es una GPU de vídeo con encoder H.264 utilizable. VAAPI, QSV y
NVENC no deben formar parte del camino crítico de este despliegue.

La aceleración hardware queda como extensión futura para otros hosts. En este
servidor la reducción real vendrá de evitar recodificaciones y controlar el
trabajo software.

### 2.6 Persistencia original

Aunque el servidor aceptaba `DATABASE_URL`, `Database::connect` siempre creaba
un `SqlitePool`, activaba WAL, configuraba un timeout busy de cinco segundos y
limitaba el pool a cinco conexiones. La auditoría de la revisión original
encontró:

- 44 migraciones específicas de SQLite.
- 165 inicializaciones `sqlite::memory:` en tests.
- Unas 294 referencias dependientes del dialecto: `SqlitePool`,
  `QueryBuilder<Sqlite>`, `?1`, `COLLATE NOCASE`, `INSERT OR IGNORE`,
  `last_insert_rowid()` y `PRAGMA`.
- Conversores ligados a `SqliteRow` y SQL directo todavía presente en
  `jellyrin-api`.
- JSON, fechas, booleanos y UUID persistidos principalmente como `TEXT` o
  `INTEGER`, desaprovechando tipos e índices nativos.

SQLx activa además sus features por defecto. El binario termina enlazando
SQLite 3.46.0 mediante `libsqlite3-sys` 0.30.1. Esa versión está dentro del rango
afectado por el bug WAL-reset documentado posteriormente por SQLite. Mientras
exista cualquier despliegue SQLite, actualizar el SQLite embebido es una tarea
de seguridad previa al resto del plan.

### 2.7 Cuellos de botella del catálogo

La migración debe rediseñar las consultas, no traducirlas literalmente:

- `replace_remote_media_library_snapshot` borra todos los items de una
  biblioteca y los vuelve a insertar uno a uno dentro de una transacción.
  Además de ser costoso, puede activar cascadas y perder relaciones o estado.
- La búsqueda por nombre usa `LIKE '%texto%'`, que no aprovecha un B-tree
  convencional.
- Los filtros de géneros y tags expanden `metadata_json` con `json_each` y
  recorren filas del catálogo.
- Varias rutas obtienen colecciones completas y filtran o transforman después
  en Rust.
- El progreso FFmpeg puede provocar una escritura por cada actualización de
  `-progress`, cuyo periodo por defecto es corto.
- Los índices actuales de `media_items` cubren identidad, visibilidad y orden
  reciente, pero no búsqueda infija ni facetas de metadata.

No hay una base de producción dentro del workspace, por lo que todavía no se
pueden afirmar ganancias reales. La línea base se construirá con snapshots
anonimizados o sintéticos de 10 000, 100 000 y 500 000 items.

## 3. Arquitectura objetivo

Antes de agregar flags aislados, conviene separar decisión, configuración y
ejecución.

### 3.1 Configuración cargada una vez

**Estado: parcial, con la carga de bajo nivel ya consolidada.** Preset e hilos
de encode forman ahora un único `HlsEncodingConfig`; límites agregados/por
carril, cola de probes, timeout y niceness forman un único
`MultimediaProcessConfig`. El servidor materializa ambos valores explícitamente
antes de validar FFmpeg y aceptar tráfico, y `HlsTranscodeRequest` permite
recibir la configuración de encode de forma explícita sin releer el entorno.
Persisten varios `OnceLock` de política en la capa API —modo, cola general,
readrate, retención, cuota, idle timeout y ventana Live HLS—. Mover ese segundo
bloque a una configuración de arranque propiedad de `AppState`, propagada a
los productores asíncronos, sigue siendo deuda de mantenibilidad; no bloquea
los límites operativos actuales.

Crear una configuración validada al arrancar y guardarla en `AppState`, por
ejemplo:

```rust
struct TranscodeConfig {
    video_preset: X264Preset,
    video_threads: Option<u16>,
    max_video_encodes: usize,
    max_audio_encodes: usize,
    max_remuxes: usize,
    max_probes: usize,
    max_auxiliary_jobs: usize,
    vod_readrate: Option<f32>,
    vod_initial_burst_seconds: u32,
    idle_timeout_seconds: u64,
    live_hls_window_segments: Option<usize>,
    completed_hls_retention_minutes: u64,
}
```

No se debe leer el entorno por separado en cada endpoint. Los valores deben
validarse al arranque, tener límites razonables y mostrarse sin secretos en los
diagnósticos.

### 3.2 Decisión explícita por sesión

**Estado: implementado en comportamiento; tipado interno pendiente.**
`DeliveryMode`, los modos por stream y la decisión direct/remux/transcode
existen. `CodecProfiles` cubre resolución, bitrates, profile, level, bit depth,
frame rate y canales, y devuelve razones deterministas. Esas razones aún son
strings compatibles con el contrato Jellyfin; convertirlas además en un enum
interno sería hardening de mantenibilidad, no una carencia funcional actual.

La decisión está separada de los argumentos FFmpeg con esta forma lógica:

```rust
enum DeliveryMode {
    DirectLocal,
    DirectProxy,
    HlsRemux,
    HlsTranscode,
    Unsupported,
}

enum StreamMode {
    Copy,
    Encode,
    Drop,
}

struct TranscodeDecision {
    delivery: DeliveryMode,
    video: StreamMode,
    audio: StreamMode,
    reasons: Vec<&'static str>,
    output: PlaybackOutputConstraints,
}
```

La API decide usando fuente, streams seleccionados, subtítulos, perfil del
cliente, bitrate y resolución. `jellyrin-core` solo transforma una decisión ya
validada en argumentos FFmpeg. La respuesta Jellyfin debe reflejar el modo real
en `SupportsDirectStream`, codecs y `TranscodeReasons`.

### 3.3 Coordinador central de trabajos

**Estado: implementado en el ciclo de vida de procesos; validación con carga real
pendiente.** Ya existen cupos separados, cola acotada, RAII, deduplicación,
cancelación, watchdog, coalescing de progreso, captura y líneas acotadas,
process groups Unix y kill/reap del hijo. La parada envía `SIGTERM` al grupo,
espera una gracia corta, escala a `SIGKILL` y recolecta el proceso. Falta medir
con fuentes/wrappers reales que no queden descendientes. La auditoría local de
paths de salida/cleanup ya está cerrada: cualquier borrado HLS exige un hijo
directo real del root, rechaza traversal y symlinks, y confirma el parent
canónico antes de `remove_dir_all`.

El `TranscodeCoordinator` compartido gestiona:

- Un semáforo agregado que limita todos los procesos FFmpeg/ffprobe entre
  carriles.
- Semáforo de recodificación de vídeo.
- Semáforos separados de audio, remux, trabajo auxiliar y probes. Todos los
  `ffprobe`, tanto locales como remotos, adquieren el carril Probe y el mismo
  cupo agregado process-wide que FFmpeg. La ruta remota conserva además su
  cola externa por proveedor, pero transfiere un único permiso central a DB y
  no hace doble admisión.
- Registro de sesión y último acceso a playlist/segmento.
- Cancelación y timeout de inactividad.
- Métricas de cola, modo elegido, proceso, velocidad y resultado.
- Adquisición de permisos por todos los puntos que ejecutan FFmpeg, incluidos
  segmentos bajo demanda y trabajos auxiliares pesados.

Los permisos deben vivir tanto como el proceso y liberarse por RAII incluso si
falla el spawn, se cancela la petición o termina con error.

El coordinador además:

- Lanza cada FFmpeg en un grupo de procesos controlable.
- Intenta cierre limpio, espera un plazo corto y envía kill forzado como
  último recurso.
- Limita y redacta stdout/stderr, incluida una cota por línea.
- Reduce escrituras de progreso a una cada 2–5 segundos, más las transiciones
  de estado y el valor final.

La cuarentena Live, la limpieza terminal y el scanner de huérfanos usan la
misma guarda de root. Un path persistido fuera del root, anidado, con `..` o
symlink falla cerrado; un path inexistente es un no-op silencioso.

### 3.4 Propiedad y consistencia del estado

| Estado | Propietario | Persistencia |
| --- | --- | --- |
| Usuarios, catálogo, configuración, listas y progreso | PostgreSQL | Duradera |
| Historial y resultado de transcodes | PostgreSQL | Duradera y acotada por retención |
| Caché/rate limits distribuidos futuros | Redis solo si se reabre el no-go | Efímera, con TTL |
| PID, `Child`, canales de stop y emisor live | Nodo Jellyrin propietario | Memoria local |
| Segmentos HLS | Nodo propietario | Filesystem temporal con cuota |
| Contenido multimedia | Proveedor externo | No se duplica en Jellyrin |

`play_session_id` ya identifica la sesión local. El `node_id` y ownership
multinodo de este diseño todavía no existen; si se implementan, PostgreSQL podrá
indicar qué nodo posee una sesión, pero no transferir el handle del proceso. Una
instalación multinodo necesitará afinidad de sesión o routing al nodo
propietario mientras HLS siga en almacenamiento local.

### 3.5 Frontera con plugins externos y MAGSTV

El proveedor MAGSTV vive y se versiona de forma independiente en
`https://github.com/alseif0x/jellyrin-plugin-magstv`. La migración de base de
datos y la optimización de FFmpeg no deben volver a introducir código específico
del servicio dentro del servidor público.

La frontera estable queda así:

- El plugin posee autenticación y protocolo del proveedor, normalización de
  catálogo, referencias opacas del proveedor, egress aislado y resolución JIT
  de la URL de reproducción firmada. La publicación MAGSTV actual omite URLs de
  playback y artwork remoto y usa referencias opacas autenticadas.
- Jellyrin posee los contratos neutrales de plugin, catálogo persistido,
  decisión direct/remux/transcode, proxy seguro, FFmpeg y estado de sesión.
- Un tuner `plugin:<id>` cifra `Username`/`Password` antes de cualquier RPC y
  conserva separado el `SecretReference` opaco propio del provider. `Type`
  vacío/desconocido falla cerrado en API, PostgreSQL y SQLite. Todo runtime
  `ExternalProcess` con `LiveTvProvider` queda sujeto al detector y al rechazo de
  secretos en configuración genérica aunque omita `ProviderSecrets`. Para
  descifrar JIT sí debe solicitar ese permiso en el manifest y recibir la
  concesión de un administrador; el grant queda ligado a
  plugin/tuner/acción/id/revisión y nunca forma parte de `TunerConfig` durable.
- El transporte limpia buffers de entrada/salida y redacta `Debug`; la
  paginación no clona el grant por página. Toda invocación que contiene un grant
  usa el lane `provider-secret` y un proceso one-shot; el catálogo conserva ese
  proceso únicamente durante sus páginas y playback tampoco reutiliza un host.
  Cada import tiene deadline global de 120 segundos, máximo 256 páginas,
  100.000 canales, 10.000 categorías, tokens de 4 KiB, 1 MiB por RPC y 64 MiB
  agregados de JSON codificado; esta última cifra no equivale a un límite RSS.
  Un lock R/W por identidad normalizada del plugin mantiene la lectura desde la
  recarga canónica hasta el fin de la RPC; revocación, rotación y mutaciones de
  tuner/plugin toman el writer hasta invalidar hosts, cerrando el TOCTOU. Estas
  medidas reducen la residencia pero no prometen zeroization completa fuera de
  las asignaciones controladas por Jellyrin.
- Cada host externo lidera un process group Unix. El cierre intenta primero RPC
  `Shutdown`, después `SIGTERM` con gracia acotada y finalmente `SIGKILL`; un
  timeout o `Drop` invalida el transporte, mata el grupo y recolecta al líder.
- Los DTO y traits compartidos no exponen `PgPool`, filas SQLx, claves Redis ni
  detalles del dialecto. El plugin compila sin cliente PostgreSQL o Redis.
- El catálogo solo persiste una referencia opaca y metadata normalizada. Tokens,
  cookies de sesión y URLs firmadas no se guardan en PostgreSQL, Redis, logs ni
  claves de deduplicación.
- El detector fail-closed reconoce claves de credencial comunes y secretos
  embebidos en userinfo o query de URLs. La salida se comprueba con canarios y
  cada canal se proyecta a un esquema seguro sin `ImageUrl`/`MediaStreams`, con
  textos públicos, `ProviderIds` y categorías acotados y sin controles ni
  valores URL.
- El entorno de un `LiveTvProvider` no admite prefijos ni nombres exactos con
  forma de credencial de cuenta (`USERNAME`, `PASSWORD`, `API_KEY`,
  `ACCESS_TOKEN`, `SECRET_KEY`, etc.). Variables exactas de identidad de
  dispositivo o protocolo continúan sujetas a revisión del paquete controlado;
  esto es reducción de exposición accidental, no una sandbox para código nativo.
- Cada inicio o renovación de playback solicita al plugin una resolución JIT. La
  respuesta tendrá expiración acotada y permanecerá en memoria solo durante la
  sesión; Jellyrin aplicará validación SSRF y redacción antes de consumirla.
- El modo de entrega se decide después de resolver la fuente, usando metadata de
  streams y capacidades del cliente. Una fuente MAGSTV no implica por sí misma
  transcodificación.

El repositorio principal fijará una revisión compatible del contrato público y
CI probará dos matrices: servidor con providers neutrales y plugin MAGSTV contra
esa misma revisión. Los cambios incompatibles usarán versionado explícito y una
ventana de compatibilidad; no se copiará código privado para hacer pasar CI.

`ExternalProcess` ejecuta código nativo y, por tanto, es una frontera de
confianza, no un sandbox. El checksum SHA-256 aporta integridad respecto al
catálogo fijado, pero no sustituye firma ni aislamiento: solo se instalarán
artefactos reproducibles del repositorio controlado. El hijo hereda el cgroup,
capabilities eliminadas, filesystem read-only y `NoNewPrivileges` del servicio,
pero comparte UID con Jellyrin. Antes de aceptar plugins de terceros se exige
firma de artefactos y proceso/contenedor con UID, filesystem y red separados.
El parche del runtime MAGSTV que valida y consume el grant, elimina su fallback
`MAGSTV_SECRET_*` y exige `Network` + `ProviderSecrets` está aplicado al árbol
MAGSTV: `origin/main` `2700d7f` está integrado por `43551fe`, la adaptación
`ExternalProcess` local es `8ce47b4` y 0.1.1 queda en `9596f1c`. La matriz contra
SDK/RPC local pasa 91/0/4 ignoradas con fmt/diff/clippy verdes. El ZIP AArch64
0.1.1 está validado, instalado y activo en staging, aunque su salud es
`Degraded` porque aún no existe tuner ni están completos egress/secretos
operativos. El pin público sigue viejo. Hasta resolver esos prerrequisitos,
crear el tuner con una cuenta controlada y ejecutar el E2E real, la frontera no
se considera cerrada.

## 4. Fase 0 — Medición y guardas de emergencia

Esta fase reduce el impacto inmediato y crea una línea base antes de cambiar la
compatibilidad.

### 4.1 Instrumentación mínima

**Estado: parcial y operativo.** `/System/Diagnostics` ya expone, sin URLs ni
credenciales, los pools API/worker y su salud, agregados de sincronización y el
último run redactado, así como admitidos inmediatos/en cola, rechazos, timeouts
y buckets de espera para FFmpeg y probes locales/remotos. También informa modo/fase y
fallback efectivo de las ejecuciones observadas. La latencia por operación SQL
y CPU/RSS/`speed` del proceso ya están instrumentadas con cardinalidad acotada;
queda pendiente la telemetría operacional del propio servidor PostgreSQL.

Registrar, sin incluir URLs ni credenciales:

- `delivery_mode`: direct, remux, partial-transcode o full-transcode.
- Razones de la decisión.
- Códecs de entrada y salida.
- Resolución, bitrate solicitado y streams seleccionados.
- PID, tiempo de arranque, `fps`, `speed`, tiempo de CPU y resultado.
- Sesiones activas y tiempo de espera por cupo.
- Bytes de HLS temporales, headroom reservado, capacidad disponible y fallos de
  escaneo del monitor compartido.
- Latencia de consulta por operación lógica, no el SQL ni sus parámetros.
- Tiempo esperando conexión, conexiones activas/idle y errores SQLSTATE.
- Filas recibidas/escritas y duración de cada sincronización de proveedor.
- Hit/miss, bytes y errores de Redis únicamente si se reabre el no-go y se
  integra una caché concreta.

Añadir contadores a `/System/Diagnostics` o a un endpoint interno equivalente.
Nunca registrar `RemoteSourceUrl`, porque las URLs Xtream contienen usuario y
contraseña.

Usar histogramas con cardinalidad acotada. `user_id`, `item_id`, URL, query y
`play_session_id` no deben ser labels de Prometheus. Sí pueden aparecer en
traces o logs protegidos y muestreados, con identificadores hasheados cuando sea
necesario correlacionar.

### 4.2 Límite de concurrencia

Variables iniciales recomendadas para este host:

```text
JELLYRIN_FFMPEG_MODE=remux-only
JELLYRIN_MAX_FFMPEG_JOBS=1
JELLYRIN_MAX_VIDEO_TRANSCODES=1
JELLYRIN_MAX_AUDIO_TRANSCODES=1
JELLYRIN_MAX_REMUXES=1
JELLYRIN_MAX_AUXILIARY_FFMPEG_JOBS=1
JELLYRIN_MAX_PROBE_JOBS=1
JELLYRIN_MAX_QUEUED_FFMPEG_JOBS=8
JELLYRIN_TRANSCODE_QUEUE_TIMEOUT_SECONDS=15
JELLYRIN_MAX_QUEUED_PROBES=8
JELLYRIN_PROBE_QUEUE_TIMEOUT_SECONDS=10
JELLYRIN_FFMPEG_NICE=10
JELLYRIN_TRANSCODE_THREADS=2
JELLYRIN_TRANSCODE_PRESET=ultrafast
JELLYRIN_TRANSCODE_MAX_BYTES=4294967296
JELLYRIN_TRANSCODE_RESERVATION_BYTES=67108864
JELLYRIN_MAX_REMOTE_PROBES=1
JELLYRIN_MAX_QUEUED_REMOTE_PROBES=8
JELLYRIN_REMOTE_PROBE_QUEUE_TIMEOUT_SECONDS=10
JELLYRIN_FFPROBE_TIMEOUT_SECONDS=15
```

Valores permitidos para preset: `ultrafast`, `superfast`, `veryfast`, `faster`,
`fast` y `medium`. El valor se convierte a enum; no se pasa texto arbitrario.
Cada trabajo adquiere conjuntamente su carril específico y el cupo agregado,
sin permitir que una espera en un carril saturado reserve capacidad global de
otro. Para x264,
`JELLYRIN_TRANSCODE_THREADS=N` genera `-threads:v N` y limita además filtros
simples y complejos con `-filter_threads N` y `-filter_complex_threads N`.
Estos flags de filtros se omiten en remux, video-copy y audio-only.
En Unix, todos los procesos FFmpeg se lanzan con niceness 10 por defecto para
que la API conserve capacidad de respuesta bajo carga. Se acepta un valor entre
0 y 19; `off` desactiva este ajuste. El cgroup continúa siendo el límite duro.

El comportamiento cuando no haya cupo debe ser configurable y compatible con
los clientes: espera corta con timeout y error explícito, o fallback a direct
stream si es válido. No se debe dejar la petición esperando indefinidamente.
El cupo de espera es distinto del cupo activo: `0` significa fail-fast y evita
que una ráfaga cree miles de futures retenidos. `ffprobe` se ejecuta con un solo
hilo, hereda la niceness de FFmpeg, limita su JSON a 8 MiB y siempre mata y
recolecta el hijo si expira el deadline o se cancela el caller.

### 4.3 Ritmo de VOD

**Estado: implementado; falta ajuste con fuentes reales.** Las entradas VOD
finitas y remotas usan por defecto:

```text
-readrate 1.10 -readrate_initial_burst 15
```

Debe colocarse antes del `-i`, porque es una opción de entrada. El valor exacto
se validará con pruebas; un margen de 1.05–1.25 permite generar algo por delante
sin codificar la película completa de inmediato. No aplicar un readrate bajo a
una fuente realmente live, porque puede perder paquetes.

Esta medida reduce el pico sostenido, pero no el coste total de una
recodificación. Si la máquina no alcanza velocidad 1x, el límite no soluciona el
problema y debe reducirse calidad o evitar la recodificación.

### 4.4 Watchdog de sesión

Actualizar `last_access_at` cuando el cliente solicita master playlist, media
playlist o segmentos. Detener FFmpeg si:

- No hay peticiones durante `JELLYRIN_TRANSCODE_IDLE_TIMEOUT_SECONDS`.
- La sesión recibe una orden de cierre.
- El usuario cambia de stream o realiza un seek que reemplaza la sesión.
- El proceso queda vivo sin una sesión registrada.

El valor inicial sugerido es 60 segundos para VOD y un margen mayor configurable
para Live TV. El watchdog debe limpiar proceso, registro, permisos y archivos.

### 4.5 Límites del sistema

`CPUQuota` en systemd o `cpus` en Compose son cinturones de seguridad, no una
optimización. Aplicarlos solo después de medir que FFmpeg mantiene `speed >= 1x`.
También añadir límites de PIDs, memoria y rotación de logs, evitando matar el
servidor por un OOM provocado por un proceso hijo.

### 4.6 Línea base reproducible

Crear un harness que ejecute, sobre el mismo host y dataset:

- Importación inicial y resincronización sin cambios.
- Resincronización con 1 %, 10 % y 50 % de cambios.
- Página inicial, búsqueda, filtros, “últimos añadidos” y EPG actual.
- Inicio, seek y parada de cada modo de reproducción.
- Una y varias peticiones concurrentes dentro de los límites soportados.

Guardar commit, configuración, versión de PostgreSQL/FFmpeg, forma del dataset y
resultados. No comparar SQLite y PostgreSQL con esquemas o consultas distintos
sin identificar también qué parte de la mejora viene del rediseño.

## 5. Fase 1 — Direct stream por proxy

Es el cambio de mayor impacto: una sesión compatible no debe iniciar FFmpeg.

### 5.1 Proxy remoto para VOD y series

**Estado: implementado y validado en staging; clientes/concurrencia pendientes.**
El proxy aplica backpressure, rangos, DNS pinning, SSRF y cancelación. MAGSTV y
Xtream integrado usan referencias opacas/JIT para publicaciones nuevas. VOD y
Live TV Xtream reales ya pasaron direct/remux, y los audits DB/logs/argv están
limpios. Cualquier snapshot legacy futuro vuelve a exigir reimport y repetición
de esos gates; faltan clientes y carga concurrente/sostenida.

La ruta autenticada de direct stream para elementos Xtream hace:

1. Cargar `RemoteSourceRef` y resolver la fuente JIT con la configuración del
   proveedor; `RemoteSourceUrl` solo se admite en el dual-read legacy hasta el
   reindex.
2. Validar que el host coincide con el proveedor configurado. Si existe
   `direct_source`, aceptar solo igualdad semántica de `reqwest::Url` con la
   fuente reconstruida; normalizaciones equivalentes como `:443` explícito no
   deben producir un falso rechazo.
3. Reenviar `GET` y `HEAD` con `Range`, `If-Range` y cabeceras necesarias.
4. Propagar estado `200/206/416`, `Content-Type`, `Content-Length`,
   `Content-Range`, `Accept-Ranges`, `ETag` y `Last-Modified` cuando existan.
5. Transmitir el body con backpressure, sin almacenarlo completo en memoria.
6. Cancelar la petición upstream cuando el cliente se desconecte.
7. Reutilizar un `reqwest::Client` compartido con timeouts y redirects
   deshabilitados.
8. Entregar a FFmpeg/ffprobe un relay loopback con token efímero para que la URL
   autenticada no aparezca en argv.

La `DirectStreamUrl` entregada al cliente debe apuntar a Jellyrin; nunca devolver
la URL Xtream con credenciales. Para fuentes remotas, mantener
`SupportsDirectPlay=false` si direct play implicaría exponer el origen, pero
permitir `SupportsDirectStream=true` cuando el perfil sea compatible.

### 5.2 Live TV

El proxy TS compartido existente debe anunciarse como direct stream para
clientes que acepten MPEG-TS/H.264/AAC, por ejemplo determinados clientes de TV.
Jellyfin Web normalmente necesitará HLS; para él se escogerá remux antes que
recodificación. En el perfil productivo `remux-only`, Live HLS ya fija vídeo y
audio a `copy`, se clasifica como `Remux` y queda admitido sin encoder. El modo
`enabled` usa el mismo remux como primer intento y dispone de un único retry
encode si FFmpeg termina sin haber publicado `segment_00000.ts` con contenido;
una fuente MPEG-2/AC3 desconocida conserva así compatibilidad sin pagar CPU en
las fuentes que sí pueden copiarse.

### 5.3 Matriz inicial de compatibilidad

La decisión debe respetar `DirectPlayProfiles` y ampliarse con condiciones del
`DeviceProfile`. Como punto de partida conservador:

- MP4 + H.264 8-bit + AAC compatible: direct proxy.
- MP3, AAC/M4A, FLAC u otro audio declarado por el cliente: direct proxy.
- MPEG-TS + H.264 + AAC/MP3 en cliente que declara TS: direct proxy.
- MKV o TS no aceptado por el navegador, pero codecs compatibles: HLS remux.
- HEVC/AV1, 10-bit, HDR o audio incompatible: evaluar transcodificación parcial
  o completa según el perfil.

### 5.4 Pruebas

- Proxy `GET`, `HEAD` y rangos válidos/inválidos.
- Upstream sin `Content-Length` y upstream que ignora `Range`.
- Desconexión del cliente cancela upstream.
- Rechazo de cualquier redirect upstream. Una allowlist same-provider sería una
  ampliación futura y requeriría volver a validar DNS/SSRF en cada salto.
- Credenciales ausentes de respuestas y logs.
- `PlaybackInfo` elige direct stream solo para un perfil compatible.
- Live TV directo reutiliza una única conexión upstream por canal.

## 6. Fase 2 — Remux y transcodificación parcial

Cuando el cliente no acepte el contenedor pero sí los codecs, usar HLS con
stream copy:

```text
-c:v copy -c:a copy -f hls
```

### 6.1 Vídeo copy

Condiciones conservadoras iniciales:

- Códec H.264.
- Bit depth de 8 bits.
- Pixel format `yuv420p`.
- Rango SDR.
- Perfil/level dentro de las condiciones declaradas por el cliente.
- Sin cambio de resolución, bitrate o frame rate.
- Sin subtítulo gráfico quemado.
- Sin filtros de vídeo.

La política debe poder ampliarse por cliente, no mediante una lista global
demasiado permisiva.

### 6.2 Audio copy

Copiar AAC-LC cuando canales, perfil y contenedor de salida sean compatibles.
Si el vídeo es compatible pero AC3/EAC3/DTS/TrueHD no lo es, usar:

```text
-c:v copy -c:a aac -ac 2
```

Esta transcodificación parcial conserva casi todo el ahorro de CPU porque evita
decodificar y recodificar vídeo.

### 6.3 Segmentación y seek

Por defecto, el muxer HLS corta en el siguiente keyframe después de `hls_time`.
No activar `split_by_time` globalmente: permite segmentos que comiencen fuera de
keyframe y puede empeorar reproducción y seek.

Con stream copy, `-ss` antes de `-i` busca alrededor de keyframes y puede no ser
exacto. Se deben probar reanudación y seek por separado. Si un cliente requiere
precisión estricta, se puede recodificar solo el tramo inicial o aceptar la
alineación al keyframe, pero no convertir toda sesión automáticamente sin
registrar la razón.

### 6.4 Fallback

**Estado: implementado y cubierto.** VOD continuo, seek bajo demanda y Live HLS
empiezan por copy/remux y, únicamente en modo `enabled`, pueden hacer un solo
intento encode si el primer intento termina antes de producir un primer
segmento no vacío. `remux-only` nunca transcodifica. Cancelación, idle, cuota y
stop no disparan fallback, y el permiso remux se libera antes de solicitar el
carril encode.

Si el remux falla antes de producir el primer segmento:

1. Marcar el intento y conservar un error redactado.
2. Reintentar una sola vez con el siguiente modo compatible.
3. Evitar bucles copy → encode → copy.
4. Liberar el permiso de remux antes de adquirir el de encode.

### 6.5 Pruebas

- Copy de vídeo y audio.
- Copy de vídeo con audio AAC convertido.
- Fallback por subtítulo gráfico, HDR, 10-bit, escala o bitrate.
- Selección correcta de índices de audio/subtítulos.
- Remux desde URL HTTP y desde stdin MPEG-TS.
- Seek sobre GOP largo y timestamps discontinuos.
- `PlaybackInfo` refleja codecs, modo y razones reales.

## 7. Fase 3 — Perfiles, calidad y razones de transcodificación

**Estado: implementado y validado localmente, incluida la suite global.**
`DirectPlayProfiles`, `TranscodingProfiles`, límites de bitrate y
`CodecProfiles` participan en la decisión. Se interpretan `Width`, `Height`,
`VideoBitrate`, `AudioBitrate`, `VideoProfile`, `AudioProfile`, `VideoLevel`,
`VideoBitDepth`, `VideoFrameRate` y `AudioChannels`, incluidos aliases de
propiedad y listas con los separadores admitidos por clientes Jellyfin.

Los operadores cubiertos son igualdad/desigualdad, any/not-any y comparaciones
menor/mayor estrictas o inclusivas. Una condición required desconocida o
imposible no se ignora: invalida el stream/perfil y termina en
`NoCompatibleStream` si no queda alternativa. Un perfil ausente conserva la
compatibilidad previa; un stream que cumple mantiene direct/copy y no introduce
CPU.

La interpretación vigente del `DeviceProfile` incluye:

- `DirectPlayProfiles` para contenedor y codecs.
- `TranscodingProfiles` para contenedor, protocolo y codecs de salida.
- `CodecProfiles.Conditions` para perfil, level, bit depth, resolución, frame
  rate, canales y bitrate.
- `MaxStreamingBitrate` y límites de usuario/sesión.
- Streams de audio y subtítulos seleccionados.

Los límites y selecciones representables —incluidos profile/level/frame rate y
canales— se trasladan a `HlsTranscodeRequest` y a la identidad de la sesión para
que dos requests con distinta salida no se dedupliquen por error. No se escala
hacia arriba: se aplica el mínimo entre fuente, cliente, usuario y configuración
del servidor.

La política evita trabajo inútil: no eleva el frame rate cuando la fuente ya
cumple el máximo y, si faltan metadatos de canales, no supone una fuente con más
de dos canales ni fuerza upmix. El parser de level acepta también H.264 1.0.

Las razones deterministas actuales incluyen:

- `ContainerNotSupported`.
- `ContainerOrCodecNotSupported`.
- `AudioCodecNotSupported`.
- `VideoProfileNotSupported`.
- `VideoLevelNotSupported`.
- `VideoBitDepthNotSupported`.
- `VideoResolutionNotSupported`.
- `VideoFramerateNotSupported`.
- `ContainerBitrateExceedsLimit`.
- `AudioProfileNotSupported`.
- `AudioChannelsNotSupported`.
- `AudioBitrateNotSupported`.
- `SubtitleCodecNotSupported`.
- `DirectPlayDisabled`.
- `NoCompatibleStream`.

El contrato Jellyfin sigue serializando strings. Introducir un enum interno que
las genere de forma exhaustiva continúa siendo una mejora de mantenibilidad,
pero ya no bloquea explicar por qué una reproducción consumió CPU ni detectar
reglas demasiado conservadoras.

## 8. Fase 4 — HLS bajo demanda, disco y sesiones largas

### 8.1 Evitar dos FFmpeg para la misma sesión

**Estado: implementado y cubierto de forma determinista.** Toda generación
on-demand comparte un lock débil y acotado por `play_session_id`, relee la
sesión canónica tras adquirirlo y conserva el guard durante stop/cleanup y la
salida completa de FFmpeg. Dos segmentos distintos tampoco pueden escribir a
la vez sobre el mismo patrón; un waiter observa el estado `completed` y no
repite la limpieza basada en un snapshot `running` obsoleto.

La implementación elegida serializa por sesión el recheck del segmento, la
recarga canónica, el stop/cleanup del proceso continuo y la generación puntual;
todo FFmpeg sigue adquiriendo el cupo agregado y el de su carril. La clave de
deduplicación incluye los parámetros que alteran la salida: encoder, modos
copy/encode, bitrate, resolución, streams, subtítulos y posición.

La generación puntual tiene además un deadline derivado de la duración pedida:
`clamp(15 + 2×segundos multimedia, 30 s, cap)`, con cap validado 30–900 s y
default 180 s. Timeout o cuota detienen y recolectan el grupo, limpian la salida,
persisten estado terminal y nunca activan el fallback encode.

### 8.2 Ventana live configurable

**Estado: rolling implementado; falta validación con clientes reales.** El
default eficiente usa `hls_list_size=20`, `hls_delete_threshold=2` y
`delete_segments+omit_endlist+temp_file`. Se conservan dos modos explícitos:

- **Timeshift completo**: `hls_list_size=0`, con cuota de disco y duración
  máxima obligatorias.
- **Ventana rolling**: `hls_list_size=N`, `hls_flags=delete_segments+omit_endlist`
  y `hls_delete_threshold` suficiente para clientes retrasados.

La ventana rolling es la opción por defecto para este servidor que solo indexa
servicios. El E2E posterior debe confirmar Jellyfin Web, Android TV y DLNA.

### 8.3 Directorio y retención

**Estado: guardas de aplicación implementadas.** El root es configurable con
`JELLYRIN_TRANSCODE_DIR`, la retención terminal parte de 60 minutos, startup y
watchdog limpian sesiones terminales/huérfanas, la cuota lógica por defecto es
4 GiB y diagnósticos exponen uso, reservas y límite. Docker usa un volumen
separado. La admisión queda serializada alrededor de una medición compartida y
cada escritor conserva una reserva RAII configurable —64 MiB por defecto—
durante toda su vida. Un único monitor de uso despierta a todos los trabajos y
evita que cada sesión recorra por separado el árbol cada cinco segundos.

La reserva es headroom de admisión, no una cuota física: FFmpeg aún puede crecer
más de 64 MiB entre dos mediciones. Por ello no sustituye un límite duro del
volumen/filesystem; ese límite y la prueba de presión concurrente se validarán
en staging. La coordinación es local al proceso; varias réplicas necesitan
roots exclusivos o una cuota física compartida. El scanner rechaza un root
symlink y no sigue symlinks internos. Todos los paths de cleanup están
confinados a un hijo directo real del root y cubiertos por tests de outside,
nested, traversal, root/candidato symlink y root relativo.

## 9. Fase 5 — Transcodificación software eficiente

Solo aplica cuando direct stream, remux y transcodificación parcial no sirven.

### 9.1 Preset e hilos

- **Estado local:** límites y observabilidad implementados; el tuning de calidad
  con fuentes/clientes reales sigue diferido.
- Default de emergencia para este host: `ultrafast`; el default definitivo se
  elegirá comparando `ultrafast`, `superfast` y `veryfast` con las fuentes y
  clientes reales.
- Dos hilos de vídeo por sesión, aplicados a encoder y grafos de filtros.
- Máximo una recodificación de vídeo simultánea.
- Mantener AAC configurable y evitar `-ac 2` si el cliente acepta el layout
  original.
- `-stats_period 2`, `-nostats`, log controlado y progreso coalescido ya están
  activos.
- El hijo usa niceness configurable y queda por debajo de la API/PostgreSQL en
  Unix; el cgroup mantiene el límite duro.

Un preset más rápido puede elevar mucho el bitrate para una calidad equivalente.
Se debe medir CPU, tamaño de salida, ancho de banda y calidad; no asumir un
porcentaje fijo de ahorro.

### 9.2 GOP alineado al HLS

Cuando se recodifica, generar keyframes alineados con la duración del segmento
para obtener segmentos independientes y predecibles. Calcular GOP usando frame
rate cuando sea fiable y validar contenido con frame rate variable. Solo añadir
`independent_segments` cuando realmente se garantice que cada segmento comienza
con keyframe.

### 9.3 Subtítulos

- Preferir entrega externa WebVTT para subtítulos de texto.
- Quemar PGS/DVDSub y otros subtítulos gráficos solo cuando el cliente no pueda
  recibirlos externamente.
- Registrar `SubtitleBurnIn` como razón, porque obliga a recodificar todo el
  vídeo.

### 9.4 Entradas remotas y probes

**Estado: implementado para fuentes Xtream; generalización pendiente.** La
concurrencia, cola, tamaño de salida, deadline, cancelación y kill/reap de
`ffprobe` están implementados. `ensure_xtream_remote_media_info` persiste
`RemoteMediaProbe` con `SourceRevision`, `ProbeVersion`, estado y
`DateLastProbed`, deduplica por fuente e invalida cuando cambia la revisión. Lo
pendiente es normalizar este contrato para otros proveedores y reforzar su
atomicidad, no volver a implementar la persistencia Xtream.

- Ejecutar `ffprobe` de forma lazy o durante una cola de indexación limitada;
  nunca lanzar cientos de probes en paralelo durante un sync.
- Deduplicar probes concurrentes por fuente y guardar su resultado en
  PostgreSQL con `probed_at`, fingerprint y versión del probe.
- Invalidar el probe cuando cambie el identificador remoto, tamaño, ETag o
  metadata equivalente; usar TTL cuando el proveedor no ofrezca fingerprint.
- Aplicar timeouts de conexión y lectura inactiva a HTTP. Reintentar únicamente
  operaciones idempotentes y con backoff acotado.
- Para live, distinguir reconexiones aceptables de una fuente terminada. Para
  VOD, respetar rangos y no reiniciar desde cero silenciosamente.
- Validar esquemas y hosts antes de entregar una URL a FFmpeg. Nunca construir
  una línea de shell: programa y argumentos seguirán siendo campos separados.

### 9.5 Arranque y compatibilidad de FFmpeg

Al iniciar Jellyrin, el comportamiento implementado es:

1. Ejecutar un probe corto de `ffmpeg -version` y capacidades necesarias.
2. Abortar el arranque antes del bind si el runtime obligatorio no está
   disponible; `/readyz` no vuelve a sondear FFmpeg dinámicamente.
3. Registrar versión y modo sin imprimir configuración sensible.
4. Verificar en runtime el manifest finito enumerable de protocolos, demuxers,
   muxers, bitstream filters y el decoder AAC. FFmpeg 8 ya no enumera parsers
   por CLI, por lo que el gate reproducible los extrae y valida desde
   `-buildconf`; el artefacto de release rechaza cualquier encoder u otro decoder.
5. Mantener el resultado en memoria; no ejecutar detección por petición.

**Supply chain AArch64 cerrada; AMD64 nativo pendiente:** la
imagen fija bases por digest y snapshot Debian. El candidato actual usa Rust
1.94/SQLx 0.9, por lo que `rsa` ya no existe en el lock, y compila la revisión
oficial FFmpeg `1e027914...` desde un archivo fijado por SHA-256. El build
comprueba los 16 patches oficiales asociados a los HIGH de NVD para 8.1.2 y
demuestra que están presentes en la fuente; el scanner consulta NVD y falla
ante cualquier HIGH/CRITICAL no mapeado. CI mantiene el corpus MP4, Matroska y
MPEG-TS, SBOM SPDX/CycloneDX, cargo-audit con RustSec fijado y Trivy de imagen
con base viva. Las excepciones gobernadas siguen vacías. El cierre vigente es
la imagen Docker ARM64 nativa de `630a430`, id
`e561d9fe178ac08515082dfdff2772b206b326aaa720c4d44ef264af19058956`,
88.538.826 bytes, con el healthcheck del Dockerfile preservado. Pasó el corpus
MP4/MKV/MPEG-TS, el runtime distroless y un
stack Compose real desde base vacía hasta esquema 117; migrador `exit 0`,
servidor healthy/read-only bajo UID/GID 10001 y cero reinicios. Los bundles
`supply-chain-arm64-630a430-docker` y `vulnerability-arm64-630a430-docker` verifican
todos sus `SHA256SUMS`; RustSec, Trivy y NVD-FFmpeg quedaron `passed=true`. La
evidencia roja anterior se conserva como registro histórico. La promoción aún
exige repetir el gate en AMD64 nativo, matriz de clientes, firma y provenance.

## 10. Fase 6 — Aceleración hardware futura

No implementar VAAPI/QSV para el host actual: el dispositivo DRM es virtio y no
aporta encoder de vídeo.

Para futuros hosts, usar una interfaz de encoder y detección real:

1. Comprobar dispositivo y permisos.
2. Consultar decoders/encoders disponibles.
3. Ejecutar un probe corto de codificación, no confiar solo en que el nombre del
   encoder aparezca en `ffmpeg -encoders`.
4. Seleccionar `auto|none|vaapi|qsv|nvenc` mediante configuración.
5. Hacer fallback una sola vez a software.
6. Probar filtros, tone mapping y subtítulos por separado.

Docker deberá exponer el dispositivo correspondiente. NVIDIA queda fuera del
alcance de la imagen Debian actual salvo que se construya una imagen específica.

## 11. Seguridad y robustez del proxy

**Alcance actual:** Xtream built-in cumple en código la frontera opaca/JIT para
publicaciones nuevas. El core MAGSTV ya implementa persist-first, grant JIT,
referencias opacas, lane one-shot para toda RPC con grant y exclusión R/W entre
invocaciones y mutaciones de seguridad. La frontera alcanza a todo
`ExternalProcess` + `LiveTvProvider` aunque omita `ProviderSecrets`; el permiso
solo habilita recibir el grant. El detector ampliado, los canarios y el esquema
seguro de canales cierran las rutas genéricas de configuración/salida. El parche
del runtime externo para consumir el grant está aplicado al repositorio hermano
y validado contra el core local, pero no publicado, por lo que el cierre de
release entre repositorios sigue pendiente. El vault cifra una única copia de las credenciales con AES-256-GCM y
una clave externa; las configuraciones públicas solo conservan
`JellyrinProviderSecretRef` y la resolución ocurre justo antes del uso. El
rollout aún debe reimportar y verificar catálogos creados antes de este cambio.

El envelope incluye versión, key id, nonce aleatorio y ciphertext autenticado.
El AAD liga versión, key id, secret id y tipo de proveedor, de modo que mover un
ciphertext a otra identidad falla autenticación. El keyring distingue una llave
activa para nuevas escrituras y llaves antiguas de solo descifrado; buffers de
clave/plaintext usan zeroization y `Debug` no imprime secretos.

Las garantías de persistencia también son transaccionales:

- En PostgreSQL y SQLite, crear/actualizar el envelope y sustituir la
  configuración por su referencia ocurre en la misma transacción; un fallo de
  la segunda escritura no deja un secreto huérfano.
- El backfill toma locks sobre plugin, tuners y configuración Live TV,
  comprueba que las tres copias Xtream coincidan, crea una referencia canónica
  y reescribe todas o ninguna. Un conflicto de credenciales falla cerrado.
- La rotación bloquea las filas antiguas y re-cifra el conjunto completo en una
  transacción. Si una fila falla, ninguna rotación parcial se confirma.
- En startup se valida readiness, se rota a la llave activa y se ejecuta el
  backfill. Secretos existentes sin keyring hacen fallar el arranque.
- La llave puede llegar directa o desde fichero/keyring. La lectura real y el
  tamaño declarado se limitan a 128 KiB; en Unix se rechazan symlinks, se abre
  con `O_NOFOLLOW`, se valida el descriptor y device/inode, y se exigen permisos
  privados. La imagen usa UID/GID `10001:10001` y el montaje recomendado es
  `root:10001`, modo `0440`. Ninguna llave vive en PostgreSQL ni en el repositorio.
- Al borrar un tuner, PostgreSQL y SQLite escanean referencias exactas en
  tuners, configuraciones de plugin y configuraciones nombradas dentro de la
  misma transacción; solo borran el envelope por `secret_id + provider` si dejó
  de estar referenciado. PostgreSQL bloquea tuner/envelope y SQLite usa
  `BEGIN IMMEDIATE`, por lo que una referencia compartida se conserva.
- El arranque reconcilia también huérfanos históricos y de otros paths sobre
  las tres superficies de configuración. PostgreSQL usa una transacción
  serializable y bloquea todos los envelopes candidatos; SQLite usa `BEGIN
  IMMEDIATE`. Un JSON o `JellyrinProviderSecretRef` inválido aborta antes de
  borrar, conserva todos los candidatos y solo genera un aviso de reparación.

- No devolver ni registrar URLs Xtream completas.
- Redactar usuario, contraseña, `api_key` y cabeceras de autorización en logs y
  diagnósticos.
- Limitar el proxy a esquemas HTTP/HTTPS y hosts asociados al proveedor del item.
- No seguir redirects del proveedor; validar y fijar DNS para impedir SSRF,
  rebinding o salto a redes internas.
- Configurar timeout de conexión y cabeceras, pero no un timeout total corto
  para streams largos.
- Limitar tamaño de cabeceras y rechazar rangos múltiples inicialmente.
- Aplicar backpressure y no acumular chunks de un cliente lento.
- Compartir el cliente HTTP y pools de conexión.
- No persistir `RemoteSourceUrl` ni `RemoteMediaProbe.SourceUrl` en VOD/series,
  ni `stream_url` en canales opacos nuevos: guardar provider/tuner,
  identificador remoto y extensión, y reconstruir la URL justo antes del uso.
- La migración 106 impone para Live TV el XOR exacto: una fila tiene URL legacy
  o `ProviderReference`, nunca ambas ni ninguna. Si encuentra una fila mixta,
  aborta y exige reimportar el catálogo antes de continuar.
- Entregar las fuentes JIT a FFmpeg/ffprobe por relay loopback tokenizado; no
  pasar la URL autenticada en argv. La excepción legacy desaparece con reindex.
- Rechazar `direct_source` distinto de la URL reconstruida y filtrar artwork
  remoto que contenga userinfo, query o fragment; no convertirlo en otra vía de
  exfiltración.
- Cifrar en la aplicación las credenciales de proveedor con una clave externa a
  PostgreSQL; la base solo guarda ciphertext autenticado, versión, nonce y key
  id.
- Usar un rol PostgreSQL propietario solo para migraciones y otro rol de runtime
  sin permisos DDL. Exigir TLS si la conexión sale de la red local de Compose.
- No publicar el puerto de PostgreSQL. Si se reabre Redis en el futuro, tampoco
  publicar su puerto. Cargar secretos desde archivos protegidos o un secret
  manager, no desde el repo.
- Establecer `statement_timeout`, `lock_timeout` y límites de conexión para que
  una consulta o cliente defectuoso no agote el servidor.
- Redactar DSN y SQL bind values. Si se integra Redis en el futuro, sus claves y
  valores tampoco deben contener tokens, usuarios Xtream ni URLs.
- El tracing HTTP registra método y path, no la URI completa: las queries
  `api_key` usadas por clientes Jellyfin no llegan a spans ni access logs.

Estas garantías protegen configuraciones y publicaciones nuevas, pero no
limpian automáticamente catálogos o logs históricos. El rollout debe localizar
los tres campos legacy, reimportar las bibliotecas afectadas, comprobar argv y
logs, y rotar credenciales si detecta una exposición.

## 12. Despliegue para este caso de uso

Docker Compose es la opción recomendada para este servidor. El perfil base
contiene Jellyrin y PostgreSQL. Los profiles Redis son scaffolding dormido para
repetir benchmarks o una reevaluación futura; no forman parte de la topología
objetivo ni deben activarse con el no-go vigente.

### 12.1 Servicios y red

- `jellyrin`: API, proxy, coordinador y procesos FFmpeg.
- `postgres`: volumen duradero, healthcheck y sin puerto publicado.
- `redis`: scaffolding bajo profiles `cache`/`distributed-cache`, apagado y sin
  consumidor en la aplicación.
- `migrate`: job one-shot que ejecuta migraciones con el rol DDL antes de
  arrancar o actualizar Jellyrin.

Usar una red interna para datos y publicar únicamente Jellyrin detrás de HTTPS.
`/health` comprueba que el proceso responde; `/readyz` verifica PostgreSQL y el
historial/checksum de esquema. FFmpeg/ffprobe se validan una vez antes de abrir
el listener y un fallo aborta startup, no se refleja como una sonda dinámica de
readiness. Redis está apagado y no participa en health/readiness.

### 12.2 Recursos y volúmenes

- Eliminar `./media` si todo el contenido es externo.
- Persistir el volumen de PostgreSQL y configuración/logs imprescindibles.
- Montar transcode en `tmpfs` o volumen separado con límite de bytes; no
  compartirlo con el volumen de la base.
- Reservar CPU y memoria para Jellyrin y PostgreSQL antes de asignar capacidad a
  FFmpeg. Un cgroup del hijo no debe estrangular la API.
- Definir límites de PIDs, memoria, file descriptors, tamaño de logs y
  `stop_grace_period` suficiente para cancelar hijos.
- Ejecutar como usuario no privilegiado, con filesystem de solo lectura y
  capabilities Linux eliminadas salvo las justificadas.
- No habilitar DLNA, `network_mode: host` ni dispositivos DRM salvo necesidad
  comprobada.

PostgreSQL empezará con configuración conservadora para cuatro cores y se
ajustará con métricas, no con presets genéricos. El pool de aplicación, no
`max_connections`, limitará la concurrencia normal.

### 12.3 Configuración y secretos

Separar:

- `DATABASE_URL` de runtime y DSN de migración.
- Límites del pool API y del worker de importación.
- Sin URL Redis en el runtime actual; añadirla solo si se reabre el no-go con un
  consumidor concreto.
- Root/cuota/retención de transcode.
- Preset, threads, cupos y timeouts FFmpeg.
- Clave de cifrado de credenciales del proveedor.

Las imágenes base, PostgreSQL, FFmpeg y herramientas supply-chain ya están
fijadas por versión y, donde corresponde, digest/checksum. El candidato
distroless AArch64 `6a15aec579b8` tiene RustSec, SBOM, Trivy y NVD verdes; antes
de publicar falta repetir el mismo gate en AMD64 nativo, revisar sus artefactos,
firmar el digest y adjuntar provenance. Redis solo entra en esta política si se
reabre su decisión no-go.

Para systemd se conservará el hardening existente, pero PostgreSQL se gestionará
como servicio separado y Jellyrin tendrá directorio de transcode, límites de
recursos y permisos mínimos equivalentes.

## 13. Migración a PostgreSQL y optimización del catálogo

### 13.1 Decisión de driver

**Estado: costura de selección implementada; intercambio completo pendiente;
PostgreSQL es el único driver productivo.** El servidor sí expone un selector
explícito, pero reconocer un nombre no equivale a soportarlo en producción:

| Driver | Selectores/esquemas | Adaptador | Manager productivo |
| --- | --- | --- | --- |
| PostgreSQL | `postgresql`, `postgres`; `postgresql://`, `postgres://` | Nativo, migraciones y repositorios productivos | Conecta |
| SQLite | `sqlite`; alias deprecado `sqlite-legacy`; `sqlite:` | `SqliteDatabase` real detrás del feature `sqlite`, usado por migración/tests | Rechaza antes de conectar |
| MySQL | `mysql`; `mysql://` | Reservado, todavía sin adaptador/migraciones | Rechaza antes de conectar |

`DatabaseDriver` es `#[non_exhaustive]`, `DatabaseConfig` valida que driver y
esquema coincidan sin incluir la URL en errores, y `DatabaseManager` es el único
punto productivo que construye un backend. Solo PostgreSQL devuelve
`is_production_supported() == true`. Una URL SQLite/MySQL nunca provoca caída a
PostgreSQL, ni una caída de PostgreSQL activa SQLite.

La similitud con Laravel queda en el manager/factory y en contratos de dominio,
no en fingir que todos los motores hablan el mismo SQL. Cada driver debe tener:

- adaptador, tipos de fila y mapeo de errores propios;
- consultas, transacciones, locks e índices nativos;
- árbol de migraciones y estrategia de upgrade/rollback propios;
- la misma suite de conformidad observable para todos los contratos usados por
  el runtime.

No se usa ni se introducirá `AnyPool`, SQL con ramas por dialecto ni fallback a
otro motor. El grafo normal de `jellyrin-server` enlaza PostgreSQL pero no
SQLite; `cargo tree -p jellyrin-server -e normal -i sqlx-sqlite` no devuelve
dependencias. El feature público canónico es `sqlite`; `legacy-sqlite` solo lo
activa como alias compatible. Redis no forma parte de esta factory ni de la
interfaz de repositorio persistente.

El contrato operativo y el checklist para añadir otro adaptador están
desarrollados en [`database-drivers.md`](database-drivers.md).

La línea SQLx 0.9 actual todavía conserva `libsqlite3-sys` 0.30.x y empaqueta SQLite
3.46.0, anterior a la corrección WAL-reset. Como mitigación inmediata, el
adaptador legacy deja de activar WAL en bases persistentes y usa rollback
journal; un test impide reactivarlo accidentalmente. Sigue pendiente actualizar
la dependencia embebida cuando exista una combinación corregida compatible
antes de volver a permitir WAL. Esto sigue siendo necesario aunque SQLite no
esté en producción, porque el migrador abre la base histórica y los tests
ejercitan el adaptador. No cambia la decisión de mantener PostgreSQL como único backend
productivo actual.

### 13.2 Estructura y frontera de código

**Estado: frontera runtime extraída; partición física aún incremental.** El
árbol actual separa:

```text
crates/jellyrin-db/src/
  driver.rs                    # identidad/capacidad de cada driver
  manager.rs                   # config validada y factory productiva
  postgres.rs                  # pools/settings y adaptador PostgreSQL
  postgres_*.rs                # repositorios SQL nativos por dominio
  provider_secrets.rs          # vault neutral
  lib.rs                       # modelos/contratos y adaptador SQLite feature-gated
crates/jellyrin-db/migrations-postgres/
crates/jellyrin-db/migrations/ # esquema SQLite histórico/test
crates/jellyrin-migrate/
```

`jellyrin-api` consume operaciones y traits de dominio y ya no contiene
`sqlx::`, `PgPool`, `SqlitePool`, `QueryBuilder` ni strings SQL. Los tipos de
fila PostgreSQL son privados del adaptador y se convierten a modelos públicos.
`MediaCatalogStore` y `XtreamCatalogStore` demuestran el patrón de contratos
estrechos con implementaciones nativas PostgreSQL/SQLite.

Todavía quedan muchas operaciones como métodos inherentes de ambos adaptadores
y `lib.rs` mantiene buena parte del SQLite histórico. Esto es deuda de
modularización, no una fuga SQL hacia la API. Antes de habilitar otro driver se
extraerán contratos pequeños por usuario, configuración, playback, sesiones,
Live TV, plugins y backup, y se reutilizarán escenarios de conformidad entre
adaptadores. Un trait monolítico que exponga cada detalle volvería la frontera
frágil y no es el objetivo.

### 13.3 Pool, timeouts y errores

**Estado: configuración y pools implementados; resiliencia avanzada
pendiente.** `DatabaseConfig` valida al arranque driver/URL, tamaños y timeouts,
mantiene la URL privada y la redacta en `Debug` y errores. PostgreSQL traduce
esa política a dos `PgPool` independientes:

```rust
struct DatabaseConfig {
    driver: DatabaseDriver,
    database_url: String, // privada y siempre redactada en Debug/errores
    api_max_connections: u32,
    worker_max_connections: u32,
    acquire_timeout: Duration,
    idle_timeout: Duration,
    max_lifetime: Duration,
    api_statement_timeout: Duration,
    worker_statement_timeout: Duration,
    lock_timeout: Duration,
}
```

Los valores actuales para este host son pool API máximo 6 y pool worker máximo
2, sujetos a benchmark. Separar ambos evita que una importación retenga todas las
conexiones necesarias para login, navegación o heartbeat. El worker puede tener
un timeout largo controlado; las consultas interactivas deben fallar pronto.

Ya están configurados `application_name` por pool, timezone UTC,
`statement_timeout`, `lock_timeout`, `acquire_timeout`, `idle_timeout` y
`max_lifetime` finitos. Quedan por cerrar:

- Exigir/verificar TLS fuera de la red privada según la política del despliegue.
- Añadir `node_id` a `application_name` si aparece ownership multinodo.
- Retries con jitter solo para transacciones idempotentes ante deadlock o fallo
  de serialización; nunca reintentar ciegamente cualquier error.
- Mapeo estable de unique/FK/not-found/timeout a errores de dominio sin exponer
  SQL ni valores bind.

Las migraciones se ejecutan en un job exclusivo con rol DDL. Las instancias
de Jellyrin comprueban la versión de esquema en readiness, pero no compiten
por migrar al arrancar.

### 13.4 Esquema PostgreSQL nativo

**Estado: baseline nativa implementada; validación de escala/collation
pendiente.** El esquema PostgreSQL no copia los tipos SQLite literalmente:

| Semántica | SQLite legacy/test | PostgreSQL vigente |
| --- | --- | --- |
| IDs internos de servidor, usuario, folder, media, task y lista | `TEXT` | `UUID` |
| IDs externos, tuner, canal, programa, token y session key | `TEXT` | `TEXT` |
| Instantes | RFC3339 en `TEXT` | `TIMESTAMPTZ` |
| Flags | `INTEGER` | `BOOLEAN` |
| Contadores/rowid | `INTEGER AUTOINCREMENT` | `BIGINT GENERATED BY DEFAULT AS IDENTITY` |
| Payloads estructurados | JSON serializado en `TEXT` | `JSONB` |
| Duraciones/ticks | `INTEGER` | `BIGINT` con checks cuando proceda |

Los IDs media de 32 caracteres hexadecimales y los UUID con guiones se
normalizan a `UUID`; la API sigue serializándolos en el formato Jellyfin que
espere cada endpoint. Los identificadores Live TV generados como texto no se
fuerzan a UUID.

La baseline sustituye `COLLATE NOCASE` con índices únicos sobre
`lower(columna)` para igualdad case-insensitive y `pg_trgm` para búsqueda
parcial. Falta fijar/versionar la política de collation del orden visible y
comprobar español, acentos y caracteres no ASCII.

Ya hay constraints e índices nativos; antes del rollout hay que validar sus
planes y esa ordenación multilingüe con datos reales. No se usarán enums
PostgreSQL para estados que cambien con frecuencia; `TEXT` con check es más
sencillo de evolucionar.

### 13.5 Modelo de catálogo y secretos

**Estado: modelo base, vault y primera proyección normalizada de facetas
implementados.** `metadata JSONB` conserva datos variables del proveedor, mientras
identidad, folder, tipo, colección, bitrate, resolución, visibilidad y
timestamps ya son columnas. El estado vigente es:

- `media_items`: identidad, folder, nombre/sort name, tipo, colección, runtime,
  bitrate, resolución, visibilidad, timestamps y metadata cruda.
- `media_streams` permanece JSONB y el pushdown de idioma/subtítulos lo consulta
  de forma acotada; normalizarlo solo se justificará con planes/medidas.
- `media_item_facets` normaliza Genre/MusicGenre, Artists/AlbumArtists,
  Album/AlbumName/Albums, People/SeriesPeople/Cast, Studios/SeriesStudios,
  Tags y ProductionYear. Conserva grafía, posición, stable ID y payload; la
  tabla de aliases añade stable/imported IDs y UUID simple para Person. La PK y
  los índices permiten lookup por item/kind/value/entity sin transportar
  metadata cruda.
- `media_item_genre_selectors` conserva separadamente todos los tokens exactos
  admitidos por `GenreIds`, incluidos IDs importados sin `Name`. Así se puede
  filtrar en SQL sin inventar una faceta visible vacía ni ampliar los endpoints
  de colecciones.
- `media_item_upcoming_dates` conserva el instante efectivo de Upcoming como
  segundos Unix + nanosegundos. La fila solo existe si la primera clave presente
  entre `PremiereDate`, `AirDate` y `DateCreated` es un RFC3339 válido; así una
  clave anterior inválida no cae accidentalmente a otra y el índice ordena sin
  pérdida de precisión.
- `jellyrin_derived_projection_versions` registra extractor, fecha y conteos.
  La migración PostgreSQL 109 no inserta un falso marker desde SQL: Rust lo escribe como
  última sentencia del backfill. `EnsureCurrent` es O(1) si la versión coincide;
  `Force` se usa después de importar SQLite. Fallo, cancelación o trigger revierten
  delete, inserts y marker juntos, y runtime solo lee/verifica la versión. La
  migración 110 incorpora los selectores de género y eleva ese contrato a v2;
  la 111 incorpora las fechas Upcoming y lo eleva a v3; la 112 incorpora los
  selectores exactos Person/Studio/Tag y lo eleva a v4.
  SQLite mantiene su marker equivalente para reconstrucción única.
- Ratings, estados, jerarquía de episodios y filtros complejos todavía no tienen
  toda la superficie normalizada necesaria para retirar sus fallbacks en Rust.
- `catalog_sync_runs`: proveedor, generación, estado, conteos, timestamps y
  error redactado.
- `ProviderReference` para Live TV y `RemoteSourceRef` para VOD/episodios:
  provider/tuner, kind, remote id y extensión necesaria para reconstruir la
  URL, sin credenciales por item. El CHECK/trigger de migración 106 exige URL
  legacy XOR referencia opaca en cada canal. El preflight y las consultas de
  auditoría están en [`live-tv-source-invariant.md`](live-tv-source-invariant.md).
- `provider_secrets`: envelope AEAD versionado, key id, nonce, ciphertext,
  revisión y timestamps; la llave externa nunca se almacena en la DB.

Las publicaciones nuevas no guardan URLs Xtream completas con usuario y
contraseña en metadata. La configuración secreta se cifra en la aplicación y
la URL se construye JIT en memoria; probe y FFmpeg consumen un relay loopback.
El staging actual pasó con cero `RemoteSourceUrl`,
`RemoteMediaProbe.SourceUrl`, probes inválidos o `live_tv_channels.stream_url`
opacos. Un catálogo legacy importado en el futuro puede reintroducirlos y
requiere reimportación más repetición de los audits antes del rollout.

### 13.6 Sincronización masiva sin borrado total

**Estado: ingestión incremental, staging durable, rollout real de catálogo y
E2E HTTP direct/remux completados; clientes reales pendientes.** La migración 113 crea una generación
durable fuera del catálogo visible. El proveedor descarga el cuerpo HTTP con límite a un fichero temporal
privado, parsea el array JSON en bloques de 500 y hace append de 1–1.000 items;
no mantiene el catálogo completo en RAM ni una transacción durante la red.
PostgreSQL y SQLite conservan proyecciones, advisory locks, sync/generation ids,
tombstones y merge set-based. Películas y series del mismo refresh se publican
en una transacción corta y un fallo tardío revierte ambas bibliotecas.

El `ON CONFLICT ... DO UPDATE ... WHERE ... IS DISTINCT FROM ...` evita
reescribir filas media idénticas: un sync sin cambios conserva `updated_at` y
`last_seen_at`. El sync sí conserva su registro de auditoría y puede actualizar
metadata de la carpeta; “no-op” se refiere a no reescribir todo el catálogo.
Esta garantía tiene tests PostgreSQL y SQLite. Un runner ignorado compara la
carga actual con `COPY FROM STDIN` usando el mismo schema de staging y valida
filas insertadas. En PostgreSQL local COPY solo alcanzó 1,046x a 100k y 1,030x
a 500k; no supera el gate 2x definido para justificar otra ruta de serialización
y sus casos de escape. Por tanto, QueryBuilder permanece en producción y COPY
queda como benchmark reproducible, no como deuda pendiente.

El incidente real de staging mostró que el límite debía aplicarse a los items
seleccionados, no al array bruto del proveedor. El nuevo parser admite hasta
1.000.000 de elementos inspeccionados por biblioteca, filtra cada bloque antes
del append y valida el documento completo, incluido JSON truncado después de un
bloque válido. El límite por respuesta sigue siendo 64 MiB. En el E2E real,
Movies llegó a 39.093 items pero el catálogo Series global superó ese tamaño;
la generación se abortó sin publicación parcial. El fallback por las 24
categorías se desplegó después. Una primera validación limitada publicó 1.974
episodios; el rollout completo posterior publicó 39.093 películas y 455.520
episodios de 22.194 series en 4.969 segundos, con 3 series omitidas por fallo
remoto persistente, 0 duplicadas, pico de servicio de 158.728.192 bytes y 0
stages residuales.

No se adopta `page`/`limit` como contrato principal: no pertenece a la superficie
Xtream interoperable y algunos paneles lo ignoran. El catálogo se pagina al
cliente desde PostgreSQL después de una ingestión incremental completa. El
fallback para Series superiores a 64 MiB usa `category_id`, valida que el panel
respeta realmente el filtro y rechaza IDs repetidos/solapes; nunca asume páginas
completas basadas en extensiones no estándar.

El pipeline y sus optimizaciones restantes quedan así:

1. `[x]` Adquirir un advisory lock PostgreSQL por proveedor y biblioteca.
2. `[x]` Crear `catalog_sync_run` y obtener un `generation_id`.
3. `[x]` El payload tiene límites de tamaño y elementos y usa un parser JSON
   incremental que tolera
   cortes arbitrarios entre chunks/UTF-8, filtra mientras lee y drena hasta
   `]`/EOF para detectar respuestas truncadas incluso después de alcanzar un
   límite configurado.
4. `[x]` Medir `COPY FROM STDIN` a 100k/500k: no compensa (1,046x/1,030x
   frente a QueryBuilder), por lo que no se incorpora a producción.
5. `[x]` Rechazar IDs duplicados, campos obligatorios vacíos y provider refs no
   válidos antes de tocar el catálogo visible.
6. `[x]` Hacer `INSERT ... ON CONFLICT DO UPDATE`, actualizando solo filas realmente
   distintas con `IS DISTINCT FROM`.
7. `[x]` Marcar como missing los items no vistos en la nueva generación; no borrarlos
   inmediatamente ni activar cascadas sobre progreso/listas.
8. `[x]` Géneros/tags y las otras ocho clases base de faceta se extraen y
   actualizan de forma set-based/atómica. Los filtros equivalentes las aprovechan
   cuando su mapping coincide; Genre/Person/Studio/Tag exactos ya usan sus
   selectores. Rangos complejos y formas no equivalentes conservan fallback.
9. `[x]` Confirmar atómicamente la publicación y el sync completado.
10. `[x]` Staging durable se elimina al publicar/abortar y el GC recoge stages
    abiertos o abortados con más de 24 horas; abortos y publicación completa se
    midieron con 0 stages residuales en el catálogo real.
11. `[x]` Añadir al contrato `XtreamCatalogStore` una sesión durable
    `begin_stage / append_chunk / publish / abort`, con lotes de 1–1.000 filas
    y `sync_id`; no mantener una transacción abierta durante llamadas HTTP.
12. `[x]` Hacer que PostgreSQL y SQLite de pruebas almacenen los lotes en staging
    aislado. Sólo `publish` toma locks, valida conteos/IDs y promueve películas y
    episodios de ambas bibliotecas en una transacción corta.
13. `[~]` Incorporar abort y GC de stages huérfanos. Error de catálogo, JSON
    malformado o fallo tardío conserva intactas ambas bibliotecas visibles. Los
    detalles individuales de Series reintentan cuatro veces timeout/conexión,
    429 y 5xx con espera exponencial; un fallo transitorio persistente omite solo
    esa serie. La reanudación idempotente tras un ACK incierto requeriría
    `chunk_no + digest`; el contrato vigente es one-shot y reintenta con una
    generación nueva.
14. `[x]` El resultado conserva los cuatro contadores legacy y separa métricas
    counts-only: Movies recibidas/seleccionadas/staged/publicadas, Series
    recibidas/seleccionadas/omitidas/deduplicadas y Episodes
    seleccionados/staged/publicados. Publicado solo se marca después de
    `publish`; los límites numéricos explicitan body por clase, items
    inspeccionados, chunk de 500, append máximo de 1.000 y concurrencia de
    detalles Series=1. Los tests rechazan texto dinámico en `Metrics`/`Limits`.
15. `[x]` Probadas 100.001 filas brutas, chunks acotados, Unicode/cortes JSON,
    límite inspeccionado, duplicados globales, invisibilidad y publicación
    atómica en SQLite/PostgreSQL, además del fallback `category_id` ante un
    `Content-Length` global excesivo. El fixture valida filtro de categoría
    ignorado y solapes, y el proveedor real completó las 24 categorías.
16. `[~]` `SyncXtreamMedia` real ya creó `Xtream Movies` y `Xtream Series`,
    registró conteos no secretos y mantuvo RAM acotada. Browse API global y por
    `ParentId`, página predeterminada 25, páginas explícitas y `Limit=0` están
    validados sobre las 22.194 series. Falta validación visual y reproducción
    desde clientes Jellyfin reales.

Una sincronización fallida deja visible la generación anterior. Cancelación,
timeout o caída del proveedor no pueden producir un catálogo vacío. Los locks de
sync usarán PostgreSQL inicialmente; añadir Redis aquí aporta menos garantías y
otro punto de fallo.

Para catálogos reconstruibles, la primera instalación PostgreSQL puede ejecutar
un reindex limpio desde Xtream. Así solo se migra desde SQLite el estado local e
irremplazable y se evita transportar metadata obsoleta o URLs con credenciales.

### 13.7 Consultas e índices

**Estado: hot paths paginados, proyección Series 116 y resumen incremental de filtros 119 desplegados.**
`MediaCatalogStore` ejecuta `COUNT(*)` exacto y página en un snapshot
`REPEATABLE READ`, con cap 500 y orden estable por id. Empuja a SQL ids, carpeta
virtual/`ParentId`, tipos, colección/media, contenedor, tipo de vídeo, idiomas,
subtítulos, búsqueda de nombre, HD/4K, resolución, fechas, prefijos, played,
favorite/resumable y hasta tres campos de orden. El playback del usuario entra
por join, evitando el N+1 de ese camino. Cuando una petición debe conservar el
fallback legacy, la serialización, sus filtros de playback, Latest, vistas
especiales, elementos de playlists/colecciones y todos los resúmenes virtuales
Series/Season obtienen los estados de usuario con una consulta por lote, no una
consulta por elemento. Los browse de colecciones/playlists agrupan además sus
conteos y permisos. Parent virtual, recomendaciones de películas,
NextUp/Upcoming y temporadas limitan la lectura inicial al dominio o carpetas
relevantes; Upcoming hidrata metadata solo para sus candidatos.

El contrato `media_item_catalog_counts` comparte esos filtros y playback, pero
no pagina: agrega tipos base en SQL y hace una proyección streaming restringida
a episodios o filas que contengan Album/AlbumName/Artists/AlbumArtists/
RemoteTrailers/Trailers. PostgreSQL usa `REPEATABLE READ` read-only y SQLite una
transacción única. El acumulador común conserva `trim().to_ascii_lowercase()`,
arrays/objetos/números, precedencia de URL y overflow explícito; no transporta
`MediaItem`, streams ni metadata completa al API. Un test con 513 items prueba
que Counts no se trunca a 500, y los shapes SearchTerm/Genres siguen exactos por
fallback.

El contrato neutral `media_item_query_filter_values` elimina el scan de
`/Items/Filters` y `Filters2` para los shapes equivalentes. Una CTE de items
seleccionados reutiliza los joins/predicados del catálogo sin aplicar página ni
orden de respuesta; PostgreSQL agrega en una sentencia y SQLite ejecuta sus dos
proyecciones dentro de una única transacción de lectura. La expansión recursiva
reproduce las claves exactas, object.Name solo string, aliases de idioma,
precedencia de Url/url/Path/path, extensiones `Path::extension`, casing de
MediaTypes y primera grafía determinista. El gate autentica `UserId`, resuelve
carpeta padre más hijos y usa búsqueda sobre escalares; Sort, parent sintético,
filtros de metadata/rangos y tokens desconocidos vuelven al camino legacy. El
atajo Xtream que ignoraba predicados fue retirado; Live TV sigue especializado.

Las rutas de colecciones, by-name y stable/imported ID usan
`media_item_facet_values`, lookup normalizado y aliases indexados cuando la
consulta es global o de carpeta simple. Person conserva payload para Overview,
ProviderIds e ImageTags, y UUID imported dashed/simple. Queries con filtros,
tipos o jerarquía no modelados siguen usando el fallback exacto. Las regresiones
SQLite/PG/API cruzan 500 filas, verifican parent+hijos/otra carpeta y separan
Artists de AlbumArtists sin introducir un cap artificial.

Las validaciones de propietarios de imágenes ya no materializan el catálogo
para comprobar un UUID. `media_item_exists` y `media_item_by_id_visible`
distinguen ausencia de fallo real, filtran `missing_since` y tienen SQL nativo
por driver; SQLite resuelve en una consulta UUID simple/hyphenated y conserva la
preferencia legacy por la forma simple. Ancestors reutiliza un único snapshot
TV con metadata inline para Season/Series y un lookup puntual para items
normales. La regresión con 512 películas cubre owners normal/folder/list/
Series/Season/metadata, 400/404, orden exacto y scope cross-folder; el contrato
DB pasó también contra PostgreSQL 16.14 real en una base desechable.

La resolución de IDs sintéticos de Series ya no materializa el catálogo global:
el contrato neutral `tv_series_lookup_candidates` ejecuta una única consulta
PostgreSQL/SQLite sobre vídeos visibles de `tvshows`, `tvshow` o `series` y trae
la metadata en la misma fila. La derivación exacta del ID permanece en Rust para
conservar paths, FNV e IDs canónicos/compactos. Las pruebas excluyen 512 Movies
y verifican el mismo candidato TV y metadata inline en ambos drivers.

El browse de Series tampoco agrupa ya todos los episodios en Rust ni vuelve a
interpretar 455.520 documentos JSON por página. El contrato neutral
`tv_series_catalog_page` selecciona en SQL una página estable de claves
canónicas `SeriesId`, obtiene el total exacto y carga solo los episodios de esas
series. La migración 116 mantiene tres tablas de proyección: claves Series,
miembros y cobertura versionada. Los triggers invalidan cobertura ante cualquier
escritura directa; los tres caminos de publicación la reconstruyen dentro de la
misma transacción y solo la exponen al quedar completa. Respeta el `ParentId` de
una biblioteca virtual TV, usa 25 elementos cuando el cliente omite `Limit` y
conserva `Limit=0` como consulta de conteo sin items. Datos legacy inválidos,
nombres inconsistentes, cobertura mixta o colisiones cross-folder vuelven
explícitamente al algoritmo anterior para preservar identidad y agrupación.

El backfill PostgreSQL extrae los campos JSON una sola vez a una tabla temporal
estrecha, valida y publica desde ella; evita ordenar filas anchas o repetir scans
sobre una base físicamente dispersa. En una restauración real completó en 25,0 s
y en staging en 25,1 s, con 22.194 claves, 455.520 miembros, una cobertura exacta
y cero huérfanos. El `EXPLAIN ANALYZE` de la página principal bajó a 51 ms, 311
buffers y 2,6 MiB de sort en memoria, sin el spill de ~396 MiB del camino
anterior. Una carga HTTP de 80 páginas, concurrencia 8, produjo 0 fallos, total
22.194 exacto, p50 447,8 ms, p95 669 ms y p99 895 ms; el baseline anterior era
p95 6.169 ms. Jellyrin consumió 3.385 ms de CPU durante la carga, pasó de 4,8 a
53,5 MiB de memoria y PostgreSQL escribió 14.934.016 bytes temporales en total.
La clave API efímera quedó eliminada al terminar.

Los endpoints interactivos de similares, soundtrack, instant mix y remote
search dejaron de cargar ítems y metadata de todos los dominios. El contrato
neutral `media_items_with_metadata_by_effective_types` no aplica un límite
artificial: PostgreSQL y SQLite hacen primero un prefiltrado sargable por
`media_type`/`collection_type`, traen metadata en la misma fila y aplican después
el clasificador neutral compartido por core/API/DB. Esto evita tanto el `CASE`
calculado y el `ORDER BY` innecesario sobre el catálogo completo como las
discrepancias de extras TV entre SQL y Rust. Tiene telemetría fija
`catalog.effective_type_candidates`; las pruebas cubren visibilidad, metadata,
conjuntos exactos, nombres de directorio espaciados y el scope cross-domain de
géneros. Remote search carga Movie/MusicVideo/Book/Episode por rama, BoxSet no
consulta media y solo Album/Artist/Person/Trailer conservan los ocho tipos para
preservar la compatibilidad histórica de facets JSON arbitrarias.

El refresh de una carpeta TV dejó de releer el catálogo una vez por serie. Una
sola carga agrupa episodios por nombre en `BTreeMap` y entrega cada grupo al
refresh, reduciendo CPU de O(S×N) a O(N + episodios seleccionados) sin cambiar
el alcance legacy cross-folder ni el orden de updates.

`ParentId` solo se empuja si identifica una carpeta virtual real; con
`Recursive=true` incluye hijos. Parents Series/Season, inválidos o sintéticos
siguen en el camino legacy. El pushdown exige `Limit`; valores mayores se
reducen a 500. Live TV tiene un contrato paginado separado con el mismo máximo.

Las peticiones generales de `/Items` sin límite y filtros de personas,
estudios, tags, ratings, años/premiere, Series/Season y algunos endpoints de
sugerencias y resume complejo/sin límite todavía pueden materializar catálogos o
filtrar/ordenar en Rust. Esto no incluye ya las colecciones/filtros simples
descritos arriba. Los scans administrativos de backup/análisis/import son
deliberados; las ramas cross-domain complejas de remote search para
Album/Artist/Person/Trailer pueden seguir leyendo todos los tipos. Esos son los gaps
reales que bloquean afirmar escala general 500k; paginarlos a 500 hoy alteraría
totales, deduplicación y orden Jellyfin.

Ya existen índices parciales de items visibles para nombre/folder/fecha,
GIN `pg_trgm` sobre `lower(name)`, índices de Live TV para browse global/por
categoría y EPG por canal/ventana. No deben volver a figurar como trabajo sin
hacer; lo pendiente es demostrar que el planner los usa a escala.

`qa/postgres-catalog-benchmark.js` aporta evidencia reproducible sin tocar datos
existentes: crea un schema único, genera 10k/100k/500k filas, ejecuta 12 muestras
por escenario, captura `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` y elimina el
schema incluso ante error. El último rerun aislado sobre PostgreSQL 17.10 midió para
Movie page estos p95 current→candidate:

- 10k: 1,382→1,449 ms, 0,954×.
- 100k: 11,234→10,849 ms, 1,035×.
- 500k: 13,102→11,625 ms, 1,127×.

No hay mejora estable suficiente para justificar otro índice por este escenario;
la optimización se conserva sólo donde una proyección reduce candidatos de forma
medida.

La evidencia JSON está guardada en
`/home/ubuntu/plans/generated/postgres-catalog-benchmark.json`, con SHA-256
`e004722603cd1d3fe87f1290b0f220b5aea9ad13570bbcc86c85b068adffaab9`.
El índice candidato sigue incorporado por la migración `202608080107` tanto en
PostgreSQL como en SQLite.

El harness actual añade además una distribución de género común (20 %) y rara
(0,1 %), páginas y counts, y compara explícitamente `EXISTS` correlacionado con
el semijoin `IN (SELECT item_id ...)` mediante p50/p95/max y planes con buffers.
Ese delta todavía necesita un rerun real antes de elegir otra forma SQL; el
árbol productivo conserva `EXISTS` hasta que la medida demuestre lo contrario.

Esto es un benchmark sintético aislado, no una baseline productiva: demuestra
la selección del índice y el orden de magnitud, pero no cierra distribución
representativa, pool concurrente, RSS, sync de proveedor ni p95 E2E con
handlers/clientes reales.

Los siguientes índices o normalizaciones se decidirán con
`EXPLAIN (ANALYZE, BUFFERS)`, no por intuición. Candidatos reales:

- Relaciones e índices para tags/personas/estudios si se portan esos filtros
  fuera de JSONB; género ya usa la relación dedicada de la migración 110.
- Columnas de jerarquía Series/Season/Episode y sus órdenes estables.
- Columnas de streams consultadas con frecuencia solo si expandir JSONB aparece
  en los perfiles lentos.
- Auditar índices de FKs y estados activos usados por deletes/cleanup.
- Conservar o retirar el GIN general de metadata según planes reales; no añadir
  otro índice JSONB indiscriminado.

No añadir GIN generales a cada JSONB: aumentan escrituras y vacuum. El índice
general de `media_items.metadata` ya existente debe conservarse solo si una
consulta estable y su plan demuestran que lo aprovecha.

Queda por reescribir o medir:

- Listados para proyectar solo columnas necesarias.
- Filtros en Rust que puedan ejecutarse correctamente en SQL.
- Lecturas completas sin límite por páginas o streams.
- `COUNT(*)` redundantes cuando una ventana o caché corta sea más barata.
- OFFSET profundo por keyset interno cuando el contrato Jellyfin lo permita.

`pg_stat_statements` ya está realmente precargado en staging, la extensión está
instalada en la base `jellyrin` y registra 49 statements. Queda revisar
periódicamente las queries por tiempo total y p95, y comprobar que
estadísticas/autovacuum estén al día. Los índices
tienen coste: cada uno debe justificar lecturas ahorradas frente a escritura y
espacio.

### 13.8 Port del SQL y de las migraciones

**Estado: port productivo completado; conformidad futura incremental.** Existe
una baseline PostgreSQL legible y migraciones de hardening separadas; las 44
migraciones históricas SQLite no se reproducen sobre una base PostgreSQL nueva.
El esquema SQLite histórico permanece como entrada de `jellyrin-migrate` y como
adaptador de tests/feature.

El port se ejecutó por dominios en este orden:

1. Estado de servidor, startup y configuración.
2. Usuarios, passwords, devices y API keys.
3. Virtual folders, catálogo, metadata y playback state.
4. Sesiones, transcodes, tasks y activity log.
5. Live TV/EPG.
6. Listas, plugins, trickplay, lyrics y backups.

En cada dominio se aplicó SQL PostgreSQL nativo: placeholders `$n`, `RETURNING`,
`ON CONFLICT`, tipos de fila privados, locks y semántica NULL/cascade revisada.
La API ya no contiene SQL ni tipos SQLx.

Para próximos drivers y refactors:

- Mantener queries/migraciones nativas, sin compartir strings entre dialectos.
- Extraer operaciones inherentes restantes a contratos pequeños reutilizables.
- Ejecutar escenarios contractuales PostgreSQL y del nuevo adaptador antes de
  habilitarlo.
- Revisar aislamiento, locks, cascadas, NULLs, errores y ordenación en cada
  dialecto; la mera compilación no acredita compatibilidad.

Después de estabilizar el esquema, valorar macros SQLx con metadata offline para
consultas estáticas críticas. Las consultas dinámicas seguirán usando builders
tipados y listas blancas para columnas de orden.

### 13.9 Herramienta de migración y cutover

**Estado: herramienta implementada y ensayada; cutover real pendiente.**
`jellyrin-migrate` es un binario separado que abre SQLite como fuente y escribe
las tablas duraderas en PostgreSQL. Incluye preflight, dry-run, conversión de
tipos/UUID/timestamps, digests origen/destino, locks y reporte JSON. No hay
dual-write.

`provider_secrets` es durable en SQLite y PostgreSQL: no es caché ni una tabla
`target-only` u omitible. El migrador debe copiar secret id, provider, versión, key id,
nonce, ciphertext, revisión y timestamps, convirtiendo `BLOB` a `bytea` sin
alterar bytes ni digest. Las configuraciones que guardan la referencia se
migran en el mismo cutover y la validación debe comprobar que cada `secret_id`
referenciado existe.

`catalog_sync_runs`, en cambio, existe en ambos esquemas pero es historial de
ejecución reconstruible: se clasifica explícitamente como
`omit_operational_history`, no como tabla exclusiva del destino. Debe estar
vacía en el target de cutover igual que el resto de tablas omitidas para evitar
mezclar generaciones previas con la nueva instancia.

Una SQLite pre-vault con credenciales plaintext sigue el camino compatible: se
migran sus configuraciones y, al arrancar PostgreSQL con keyring, el backfill
crea los envelopes. Una SQLite vault-enabled migra envelopes y referencias; no
puede perderlos silenciosamente. El soporte de `Bytes` BLOB↔`bytea` pasa las
36/36 pruebas, incluido el fixture PostgreSQL real con digest tipado, bytes no
UTF-8 y las referencias plugin/tuner/Live TV.

#### Preflight

- Detener Jellyrin o activar modo mantenimiento sin escrituras.
- Actualizar primero el SQLite embebido usado por la herramienta.
- Ejecutar integrity check y comprobar versión de esquema.
- Crear una copia consistente de SQLite mediante backup API o `VACUUM INTO`;
  nunca copiar solo el `.db` ignorando WAL/SHM mientras está activo.
- Verificar conectividad, versión, extensiones, espacio y esquema vacío/compatible
  en PostgreSQL.
- Hacer `pg_dump` si el target ya contiene datos.

#### Datos a migrar

- Migración estricta: usuarios/passwords, devices/tokens, configuración,
  `provider_secrets`, playback state, listas/permisos, plugins y auditoría que
  deba conservarse; credenciales plaintext legacy pasan después por backfill
  AEAD, mientras envelopes existentes conservan sus bytes y referencias.
- Migración o reconstrucción configurable: metadata manual, lyrics, trickplay.
- Reconstrucción preferida: catálogo Xtream, EPG, probes derivados,
  `catalog_sync_runs`, sesiones activas y outputs HLS.

#### Validación

- Conteos por tabla y provider.
- Hashes deterministas por lotes de entidades críticas.
- Cero FKs huérfanas y cero IDs que no puedan normalizarse.
- Comparación de usuarios, permisos, progreso, listas y configuración campo a
  campo.
- Smoke test read-only de endpoints esenciales contra PostgreSQL.
- Reporte JSON firmado con versión de herramienta, source schema, target schema,
  conteos y excepciones; nunca incluir secretos.

#### Cutover y rollback

1. Conservar SQLite y su backup inmutables.
2. Cambiar el secret `DATABASE_URL` y arrancar una sola instancia Jellyrin.
3. Ejecutar smoke tests y una sincronización controlada.
4. Abrir tráfico y observar durante una ventana definida.

El rollback simple solo es seguro mientras PostgreSQL no haya aceptado cambios
exclusivos. Durante la ventana inicial se puede mantener Jellyrin en modo
read-only. Después, volver a SQLite implicaría perder o exportar cambios; el
runbook debe decirlo explícitamente. No llamar “rollback” a arrancar una copia
antigua ignorando escrituras nuevas.

### 13.10 Backups PostgreSQL

El manifest JSON actual de Jellyrin no es un backup completo de la base. El plan
operativo debe incluir:

- `pg_dump --format=custom` programado, cifrado y fuera del volumen principal.
- Retención diaria/semanal/mensual acorde al entorno.
- Comprobación de exit code, tamaño y checksum.
- Restore automático periódico en una base aislada seguido de tests de
  integridad; un dump no probado no cuenta como backup.
- Para requisitos de RPO bajo, backups físicos y archivado WAL/PITR administrado.
- Copia y recuperación separada de la clave que cifra credenciales.

**Implementación local (2026-08-09):** `ops/postgres/backup.sh` y su timer systemd
materializan este diseño para el backup lógico: `pg_dump` custom validado, cifrado
obligatorio con `age`, checksum, publicación atómica y retenciones diaria/semanal/
mensual. `ops/postgres/restore-drill.sh` verifica checksum y descifrado, restaura en
una base creada exclusivamente para el ensayo, valida migraciones/constraints/tablas
y siempre la elimina. Ambos usan servicios libpq y ficheros de credenciales, nunca
URLs con contraseña en argumentos. Los assets y `pg_stat_statements` de Compose pasan
3/3 pruebas estáticas/sintácticas. En staging bare-metal el timer quedó activo;
`pg_stat_statements` está precargado, instalado en `jellyrin` y registra 49
statements. El snapshot cifrado post-Xtream
`20260809T103116Z` pasó checksum, descifrado y restore real:
49 tablas, cero migraciones fallidas, cero constraints inválidos y ninguna base
temporal restante. El ensayo descubrió y corrigió que un `dbname` del servicio
libpq prevalece sobre `PGDATABASE`; el verificador ahora pasa `--dbname` de forma
explícita y su QA impide reintroducir el fallo. Falta replicar snapshots y la
identidad `age` a destinos recuperables fuera del host y repetir el drill con el
dataset representativo posterior a la importación real.

Antes de cada migración de esquema, crear un backup restaurable y documentar si
la migración es reversible. Usar patrón expand/contract para cambios online.

### 13.11 Redis: no-go actual y umbral de reapertura

No introducir Redis durante el primer cutover PostgreSQL. Activarlo únicamente
si una métrica o despliegue multinodo lo justifica.

**Decisión medida para la topología actual:** no integrar Redis. No hay cliente
Redis ni consumidor de caché en el runtime. El servicio que conserva Compose
bajo profiles existe solo como scaffolding dormido para reproducir el benchmark
o evaluar un caso nuevo; no debe activarse. PostgreSQL ya aporta persistencia y
coordinación compartida, mientras que FFmpeg, broadcasts, leases y canales de
cancelación requieren ownership local. En el benchmark aislado de 50.000
valores de 1 KiB, Redis añadió unos 12 MiB de RSS vacío y unos 80 MiB cargado;
PostgreSQL caliente sostuvo la misma escala de decenas de miles de lecturas por
segundo. El análisis completo y el runner reproducible están en
`docs/redis-decision.md` y `qa/redis-cache-benchmark.sh`.

La decisión solo se reabre con un endpoint o necesidad multinodo concreta. La
prueba A/B debe mejorar el p95 end-to-end al menos 25 % y 10 ms o reducir al
menos 30 % la carga PostgreSQL, mantener un hit ratio de 80 % y caber en menos
del 5 % de la RAM del host. Sin esos umbrales, el profile permanece apagado.

Usos candidatos únicamente si se reabre la decisión:

- Cache-aside de respuestas de catálogo costosas y no sensibles.
- Rate limit de autenticación compartido entre nodos.
- Presencia e invalidaciones best-effort.
- Single-flight distribuido para evitar stampede de una caché cara.

Usos excluidos:

- Usuarios, tokens, progreso o catálogo como fuente primaria.
- Credenciales o URLs completas de proveedor.
- Segmentos HLS, imágenes grandes o blobs multimedia.
- `Child`, PID como ownership, canales Rust o órdenes críticas fire-and-forget.

Diseño de caché:

- Claves versionadas: `jellyrin:{install_id}:v1:{domain}:{hash}`.
- TTL corto con jitter y tamaño máximo por valor.
- Invalidación después de commit, no antes.
- Fallback a PostgreSQL ante timeout; circuit breaker para no bloquear la API.
- Métricas de hit/miss, evictions, memoria y latencia.
- Límite inicial de 32–64 MiB, siempre por debajo del 5 % de la RAM del host, y
  política de eviction adecuada a una caché; ajustar solo con hit rate y
  working set reales.

Pub/Sub sirve para invalidaciones porque una pérdida queda limitada por TTL. Para
trabajo que requiera replay se usarían Streams o PostgreSQL. Los locks de tareas
críticas deben tener token, TTL, renovación y fencing; en la primera versión se
prefieren advisory locks PostgreSQL para reducir componentes.

Los handles locales de FFmpeg permanecerán en memoria. En multiinstancia se
puede publicar una solicitud de cancelación al `node_id` propietario, pero la
orden duradera y el estado final se registran en PostgreSQL.

### 13.12 Observabilidad de datos

**Estado: parcial avanzado.** Ya hay snapshots neutrales y sin secretos de
pools API y worker, health round-trip, agregados de runs de sincronización y
duración del último run. La admisión FFmpeg/ffprobe tiene contadores monotónicos
y buckets acotados. Las ejecuciones FFmpeg auxiliares —trickplay, thumbnails y
tiles— comparten admisión acotada y publican active/peak, duración y outcomes
cerrados de capacidad, timeout, output limitado, exit no-cero, I/O y cancelación
RAII, sin conservar comando, argv, path, stderr ni IDs. ffprobe registra además
active/peak y outcomes cerrados
—success, exit no-cero, timeout, output limitado, I/O, JSON inválido y
cancelación— con seis buckets fijos de duración; abarca probes locales y
remotos y nunca conserva comando, path, URL o stderr. El driver dispone además de un colector común PostgreSQL /
SQLite con operaciones enumeradas, atomics saturantes, RAII ante cancelación,
buckets fijos en microsegundos y separación API/worker; Diagnostics publica ese
snapshot sin SQL, bind values, IDs ni SQLSTATE. La instrumentación de los hot
paths seleccionados cubre autenticación token/API key, página y búsqueda de
catálogo, carpetas/conteos, metadata por IDs, progreso de transcode y las fases
publish/stage/tombstone/merge/commit del sync, con pool API/worker correcto.
Faltan las vistas operacionales del servidor PostgreSQL y validar estas señales
bajo carga real.

Exponer o recopilar:

- Pool: active, idle, waiters, acquire duration y timeout.
- Queries: duración por operación, filas y error class.
- Sync: parse/copy/merge/commit por separado, items nuevos/cambiados/missing.
- PostgreSQL: locks, deadlocks, cache hit, temp bytes, WAL, checkpoints,
  autovacuum, bloat y slow queries.
- Redis futuro, solo si se reabre el no-go: hit ratio, evictions, expirations,
  used memory y fallbacks.

No activar logging SQL con bind values en producción. `EXPLAIN ANALYZE` ejecuta
la consulta: usarlo en staging o con extremo cuidado para escrituras.

### 13.13 Pruebas de la migración

**Estado actual:** el workspace pasa 695/0/7. `jellyrin-db` pasa 169/0/4 más su doctest, usando
PostgreSQL real para selectors/manager, baseline, todos los repositorios,
catálogo paginado/no-op y vault AEAD con atomicidad. El migrador pasa 36/36,
incluido el round-trip byte-exact BLOB→`bytea`; Xtream pasa 27/27. Staging ya tiene administrador,
Xtream y catálogo real; faltan clientes, carga sostenida/concurrente y el tuner
MAGSTV con prerrequisitos/credenciales controladas.

- Tests unitarios de mapeo de tipos, UUID, timestamps, JSON y errores.
- Tests de repositorio contra PostgreSQL real en CI, no mocks de SQL.
- Fixtures SQLite de cada versión soportada por el migrador.
- Tests de idempotencia/reanudación de la herramienta.
- Concurrencia: dos syncs, login durante sync, update de progreso y cleanup.
- Fallos: PostgreSQL reiniciado, pool agotado, timeout, deadlock, disco lleno y
  migración interrumpida.
- Golden tests de ordenación case-insensitive y compatibilidad de IDs Jellyfin.
- Benchmark con 10k/100k/500k items y cambios 0/1/10/50 %.

El árbol actual ya supera los dos gates estáticos: `cargo tree` no muestra
SQLite en el grafo normal de producción y no queda SQL/SQLx directo en la API.
El harness SQLite bajo `cfg(test)`/feature `sqlite` es intencional y debe
mantenerse como adaptador real de migración/tests. Los repositorios PostgreSQL
ya tienen ejecución real y la instancia de staging usa `PostgresDatabase`;
sigue pendiente el E2E autenticado de handlers con catálogos/proveedores reales.

## 14. Validación y benchmark

### 14.1 Matriz de datos

Probar sobre catálogo vacío, 10k, 100k y 500k items:

- Import inicial, sync sin cambios y sync parcial.
- Búsqueda exacta, prefijo, infijo, mayúsculas y acentos.
- Filtros combinados de folder, colección, género, tag y tipo.
- Últimos añadidos, paginación y conteos.
- EPG por canal y ventana temporal.
- Escrituras concurrentes de progreso, sesión y sync.
- Restauración de `pg_dump` y reindex completo desde proveedor.

Recoger `EXPLAIN (ANALYZE, BUFFERS)` de la consulta representativa de cada
familia con datos realistas. Verificar planes después de `ANALYZE`; un plan sobre
una tabla vacía no valida el índice.

### 14.2 Matriz funcional de reproducción

Probar al menos:

- Jellyfin Web.
- Android TV u otro cliente de TV usado realmente.
- VOD MP4 H.264/AAC.
- VOD MKV H.264/AAC.
- Audio remoto compatible e incompatible.
- H.264 con AC3/EAC3.
- HEVC 8-bit y 10-bit.
- Live TV MPEG-TS H.264/AAC.
- Subtítulos SRT/WebVTT y PGS.
- Inicio, seek, reanudación, cambio de audio y desconexión abrupta.

### 14.3 Métricas por escenario

- Modo elegido y razones.
- Número de procesos FFmpeg.
- CPU total y por proceso.
- RSS.
- `fps` y `speed` de FFmpeg.
- Tiempo hasta primer frame/segmento.
- Bytes leídos del proveedor y enviados al cliente.
- Bytes temporales escritos.
- Buffering y errores del cliente.
- Latencia y queries PostgreSQL generadas durante la sesión.
- Escrituras de progreso por minuto.

Comparar direct proxy, remux, transcodificación parcial y completa con la misma
fuente. Usar `ffmpeg -benchmark` en pruebas controladas y métricas del proceso en
ejecución real.

### 14.4 Resiliencia y degradación

Automatizar pruebas de:

- Proveedor lento, respuesta truncada, credenciales inválidas y redirect hostil.
- `direct_source` alternativo se rechaza; variantes URL semánticamente iguales
  se aceptan sin abrir otro host/path.
- Preflight de migración 106 y reimport de catálogo legacy; después, cero
  `RemoteSourceUrl`, `RemoteMediaProbe.SourceUrl` o `stream_url` opaco en DB y
  cero URL autenticada en argv de FFmpeg/ffprobe.
- Cliente desconectado, seek repetido y cancelación durante startup.
- FFmpeg ausente, colgado, exit no-cero y proceso que ignora cierre limpio.
- Directorio HLS lleno y cleanup interrumpido.
- PostgreSQL indisponible al arrancar y reiniciado durante una petición.
- Sync cancelado antes y después de la carga staging —y de COPY si se incorpora—,
  sin cambiar generación visible.
- Si en el futuro se reabre Redis: caída o memoria agotada manteniendo lectura
  desde PostgreSQL. No es un gate del runtime actual, que no contiene cliente
  Redis.
- SIGTERM de Jellyrin con sesiones activas, sin hijos huérfanos.

### 14.5 Gates automáticos

**Estado del cierre dirigido vigente:** API pasa 353/0/3 y Xtream 27/27; ambos
tienen formato, diff-check y Clippy estricto verdes. DB conserva 169/0/4 más
doctest y el migrador 36/36 con round-trip byte-exact BLOB→`bytea`. La última
baseline completa del workspace queda como evidencia histórica y deberá
repetirse antes de release; no se mezclan sus contadores con el árbol actual.
Packaging 46/46, supply-chain 46/46, systemd runtime
13/13, systemd unit 14/14, performance/recovery 37/37 y security-hardening
16/16, además de sintaxis Node y diff-check, también están verdes. La matriz
MAGSTV 0.1.1 aplicada localmente pasa 91/0/4 ignoradas contra SDK/RPC local,
clippy, fmt y diff-check; el ZIP AArch64 validado quedó instalado y activo en
staging, aunque no existe todavía tuner MAGSTV. El core `1263334` está
desplegado con esquema 117 y carga mixta 400/16 verde; su E2E HTTP Xtream
direct/remux también es verde. El E2E con clientes y la
cuenta MAGSTV reales sigue fuera de este cierre, aunque la plataforma
base de staging ya está desplegada y saludable. Los smokes de
systemd/performance/security pasaron; `pg_stat_statements` está precargado e
instalado en `jellyrin` y registra 49 statements. Xtream está configurado con
757 canales y el backup post-Xtream `20260809T103116Z`
superó el restore drill. Ninguna de estas evidencias cierra todavía la
reproducción E2E ni los prerrequisitos/E2E de MAGSTV.

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
npm run qa:packaging-release
node qa/supply-chain.js
node qa/systemd-unit-smoke.js
node qa/performance-recovery.js
```

Añadir al gate de drivers la compilación con/sin feature `sqlite`, los tests de
`DatabaseDriver`/`DatabaseManager` y `cargo tree` demostrando que
`jellyrin-server` no enlaza `sqlx-sqlite`. Estas pruebas acreditan la frontera;
no sustituyen levantar PostgreSQL real y ejecutar migraciones/repositorios.

Agregar tests unitarios del constructor de comandos, tests de decisión por
perfil y tests HTTP del proxy. Los golden existentes deben validar contratos de
Jellyfin, no argumentos antiguos que impidan una optimización segura.

CI debe arrancar PostgreSQL real como service container, aplicar la baseline
desde cero y ejecutar repositorios/migrador. Añadir jobs separados para:

- Servidor PostgreSQL-only y comprobación de dependencias.
- Migración de fixtures SQLite.
- Tests FFmpeg cuando la imagen contenga la versión fijada.
- Compose config queda exigido en CI. Compose v2 2.40.3 ARM64 está instalado
  localmente y `docker compose config --quiet` pasa con secretos efímeros; el
  daemon queda deshabilitado fuera de las pruebas para no consumir recursos.
  Sobre la imagen ARM64 exacta `630a430` se ejecutó además el stack real desde
  volúmenes vacíos: PostgreSQL healthy, migración a 117 con `exit 0`, servidor
  read-only/no-root healthy, cero reinicios y teardown completo. Los directorios
  locales `ops/postgres` e `init` necesitan permiso de recorrido `0755` para el
  bind mount; los ficheros de configuración sensibles conservan `0600`.
- Auditoría de dependencias, imagen y secretos accidentales.

### 14.6 Objetivos provisionales de rendimiento

Estos valores son gates iniciales para el host de referencia y se recalibrarán
una sola vez después de obtener baseline reproducible:

- Con 100k items, p95 de consultas DB interactivas simples por debajo de 100 ms
  y búsqueda/facetas por debajo de 250 ms.
- Espera p95 del pool por debajo de 20 ms bajo la concurrencia soportada.
- Un sync sin cambios no actualiza filas media y completa su fase DB en menos de
  un 25 % del tiempo de la carga inicial equivalente.
- Import/merge de 100k filas en PostgreSQL termina su fase DB en menos de 30 s,
  excluyendo descarga y parseo del proveedor.
- Direct proxy crea cero FFmpeg; remux crea cero encoder de vídeo.
- Una transcodificación software admitida mantiene `speed >= 1.05x` y deja CPU
  suficiente para API/PostgreSQL según la reserva definida.
- El primer segmento HLS aparece dentro de dos duraciones objetivo de segmento.
- Stop explícito elimina el hijo en menos de 5 s; abandono silencioso dentro del
  idle timeout configurado.
- El uso temporal nunca supera la cuota; al alcanzarla se rechaza o cancela de
  forma controlada antes de afectar PostgreSQL.

Un gate solo se relaja con evidencia de dataset/cliente real y dejando registrado
el nuevo motivo. No optimizar para un benchmark sintético perjudicando el contrato
Jellyfin.

## 15. Criterios de aceptación

Leyenda: `[x]` verificado localmente, `[~]` implementado o probado solo en parte
y `[ ]` pendiente de staging/producción. La evidencia exacta está en la matriz
inicial; estos criterios no deben leerse como una declaración de rollout.

### 15.1 Persistencia y catálogo

- [x] El binario de producción usa `PgPool` y no enlaza SQLite.
- [x] El selector reconoce PostgreSQL/SQLite/MySQL, pero solo PostgreSQL es
  productivo; SQLite no es fallback y MySQL permanece reservado.
- [x] La API no ejecuta SQL, no importa SQLx y no accede a pools; toda la
  persistencia queda dentro de `jellyrin-db`/`jellyrin-migrate`.
- [x] Una base PostgreSQL vacía se crea completamente mediante migraciones; la
  baseline y el delta post-vault están cubiertos por la suite DB sobre
  PostgreSQL real: 169/0/4 más su doctest.
- [~] El migrador preserva y valida todo dato irremplazable; los catálogos marcados
  como reconstruibles se reindexan correctamente. `provider_secrets` y su
  mapping BLOB↔`bytea` pasan 36/36 con PostgreSQL real, referencias, digest y
  comparación byte-exacta. Queda el ensayo de cutover/reindex con un snapshot
  representativo.
- [x] Una sincronización fallida no vacía ni cambia la generación visible.
- [x] Un sync sin cambios no reescribe las filas media ni modifica su
  `updated_at`/`last_seen_at`; sí registra el sync y puede tocar metadata de
  carpeta.
- [~] Búsqueda, filtros, orden y IDs mantienen el contrato Jellyfin. Facetas,
  colecciones y filtros simples ya tienen contratos SQL sin cap, proyección 117
  fail-closed, resumen 118 exacto, reconciliación incremental 119 y frontera de
  publicación privilegiada 120 por
  carpeta/tipo. El despliegue conserva
  ganadores incluso con grafías distintas, valida cada item y lee el catálogo
  real resumido en p95 3,058 ms sin temporales. El browse real devuelve páginas
  de 50 con total exacto de 39.093 Movies y 22.194 Series en 0,138/0,154 s.
  El rollout 119, el CAS concurrente, el probe Xtream real y HTTP 80/8 ya están
  verdes. El esquema 120 elimina el marcador GUC y pasa ACL, spoofing y sombras
  temporales en PostgreSQL 16 aislado, y quedó desplegado en staging el
  2026-08-10 con la ACL de solo lectura y las funciones `SECURITY DEFINER`
  reverificadas sobre la base productiva. Después quedan ampliar
  el scope mixto/padre+hijos, coalescer grandes lotes y mantener fallback exacto para
  predicados complejos.
- [~] El backup se restaura en una base aislada. El timer endurecido está activo
  en staging y el snapshot cifrado post-Xtream `20260809T103116Z` pasó checksum,
  descifrado y restore de 49 tablas con cero migraciones fallidas, cero
  constraints inválidos y cleanup
  completo; además pasan 3/3 pruebas de operaciones. Falta replicar snapshots y
  clave a un destino recuperable fuera del host y conservar allí la evidencia
  periódica.
- [~] Bajo la carga objetivo no hay agotamiento de pool, deadlocks sin resolver ni
  queries interactivas que excedan su timeout para las vistas normales. La
  repetición productiva de 400 peticiones, concurrencia 16 sobre pool 6, pasó con
  0 fallos, 0 deadlocks y 0 bytes temporales; p95 Series/Movies/Live TV/Filters
  fue 1,253/0,809/0,341/5,161 s. El P0 posterior deja Movies/Filters2 en 0,618 s;
  el binario 117 medía Series/Episode en 11,1–11,8 s. El 118 desplegado baja la
  lectura DB aislada a p95 3,058 ms y cuatro concurrentes a 8,099 ms. En HTTP
  400/16 pasó con cero fallos/deadlocks/temporales y p95 de 0,222–0,227 s para
  Series. Queda resolver el scope extremo sin tipo sobre ambos catálogos.
- [~] El benchmark aislado PostgreSQL 10k/100k/500k ya registra p50/p95 y planes.
  Movie page no supera el gate con el índice candidato
  (0,954×/1,035×/1,127×); Upcoming y los selectores 112 sí usan índices para
  conducir candidatos y búsquedas PK. La evidencia está fijada
  por SHA-256 en la sección 13. Falta comparar contra snapshot/baseline y
  distribución reales, además de medir sync masivo, handlers representativos y
  concurrencia E2E antes de cerrar este criterio.

### 15.2 Reproducción

**Estado general `[~]`:** las decisiones, límites y cleanup tienen cobertura
offline. DirectProxy VOD y Live TV direct/remux ya están acreditados con una
fuente Xtream real y medidas acotadas del host; la aceptación definitiva exige
MAGSTV real, clientes Jellyfin usados en producción y medidas concurrentes y
sostenidas de CPU/`speed`.

Las condiciones de profile/level/bit depth/frame rate/channels y sus razones ya
están implementadas. Process groups, `SIGTERM` con gracia, escalado a `SIGKILL`
y reap también están implementados; lo pendiente aquí es validarlos bajo carga
y con procesos/fuentes reales, no escribir ese control de ciclo de vida.

- `[x]` Un VOD Xtream remoto compatible usa direct proxy, revalida cada salto
  CDN, conserva `Range` y no crea proceso FFmpeg: 206/65.536 bytes, ~40,7 ms de
  CPU y pico de servicio menor de 10 MiB en el probe desplegado.
- `[x]` Live TV Xtream remoto expone direct stream y HLS sin revelar la fuente:
  directo 200/112.827 bytes con 0 FFmpeg; HLS 200/1.702.152 bytes en modo
  efectivo `remux`, sin fallback, ~15,3 MiB RSS, stop/reap y leases 0→0.
- `[x]` El contrato real de Jellyfin Web para `LiveStreams/Open` acepta `ItemId`,
  `MediaSourceId` y `OpenToken` en query además del cuerpo. En staging, dos
  canales Xtream del mismo tuner entregan 200; un tercer canal concreto devuelve
  503 porque el upstream resetea su conexión antes de cabeceras. No es un fallo
  de FFmpeg, credenciales, DNS, egress ni construcción de la URL.
- `[x]` El bare-metal ARM64 usa el FFmpeg Debian con encoders software bajo un
  único job, dos threads, niceness 10 y `CPUQuota=150%`. Un episodio HEVC+AC3
  incompatible pasó de `NoCompatibleStream` a HLS y produjo el primer segmento
  en 3,424 s con 3,860 s de CPU de cgroup. Un VOD 4K HEVC+AC3 produjo segmento
  en 11,613 s con 13,677 s de CPU: demuestra compatibilidad y contención, pero
  no velocidad de tiempo real para 4K.
- `[~]` Un contenedor incompatible con H.264/AAC compatible usa remux y no contiene
  encoder de vídeo en el comando.
- `[~]` Audio incompatible con vídeo compatible usa `-c:v copy`.
- `[~]` Nunca hay más recodificaciones de vídeo que el límite configurado.
- `[~]` Un seek no deja dos encoders activos para la misma sesión.
- `[~]` Una desconexión detiene el trabajo dentro del timeout configurado.
- `[~]` Live HLS eficiente cerró una sesión real sin residuales; falta carga
  sostenida para acreditar la cota física de disco.
- `[~]` `PlaybackInfo` explica el modo y las razones reales.
- `[~]` Ningún log o respuesta expone credenciales del proveedor.
- `[~]` En este host, el servidor conserva CPU disponible durante una reproducción
  software por la cuota física. 1080p medido queda cerca del tiempo real; 4K
  software no cumple todavía `speed >= 1x` y requiere worker externo o hardware
  de vídeo real, no ampliar la cuota del host índice.
- `[~]` Un cierre explícito real terminó y reaprovechó/eliminó el proceso; falta
  validar desaparición silenciosa e idle timeout con fuente real.
- `[~]` El stop Live real dejó cero procesos y leases; faltan error, SIGTERM y
  carga concurrente para cerrar todos los caminos de cleanup.
- `[~]` Las escrituras de progreso están acotadas y no saturan PostgreSQL.

### 15.3 Seguridad y degradación

- [~] Xtream built-in ya publica referencias JIT; el core MAGSTV ya entrega un
  grant JIT persist-first exclusivamente en procesos one-shot, protege
  revocación/rotación con un lock R/W por plugin y aplica detector/canarios más
  un esquema seguro de canales a todo `ExternalProcess` + `LiveTvProvider`. El
  parche del runtime externo para consumir y validar el grant y retirar
  credenciales de cuenta por entorno está aplicado y validado contra el core
  local con 91 pruebas aprobadas, 0 fallidas y 4 ignoradas, además de
  clippy/fmt/diff verdes. Integra `origin/main` `2700d7f` por `43551fe`, la
  adaptación `ExternalProcess` local `8ce47b4` y 0.1.1 `9596f1c`, pero el pin
  público sigue viejo. El ZIP 0.1.1 quedó instalado y activo en staging tras
  refrescar el repositorio; su configuración admin responde sin devolver
  credenciales, pero aún no existe un tuner MAGSTV.
  El vault AEAD, readiness,
  backfill, rotación y escritura/configuración atómica están implementados y
  cubiertos en DB local. Xtream está configurado en staging y ha indexado 757
  canales; su direct/remux HTTP real está validado, pero la reproducción E2E
  con clientes Jellyfin reales continúa pendiente. La
  UI MAGSTV fue corregida para aceptar solo credenciales; no quedan cubiertos
  por ello egress, secretos operativos, cuenta/tuner ni E2E real.
  La migración 106 impone URL legacy XOR referencia opaca en Live TV y ya pasó
  en PostgreSQL real. Falta publicar el plugin, actualizar su pin tras publicar
  el core y ejecutar el E2E actualmente diferido. Los audits actuales de
  DB/logs/argv están completos y limpios; cualquier catálogo legacy futuro
  obliga a reimportar y repetirlos antes de declarar completado el rollout.
- [x] PostgreSQL no publica puerto y usa roles/secretos separados; Redis está
  apagado y su scaffolding bajo profiles tampoco publica puerto.
- Redis no es una dependencia actual. La degradación a PostgreSQL solo será un
  criterio si se reabre e integra una caché concreta.
- [~] Una caída de PostgreSQL produce errores controlados y readiness false; no se
  sirven respuestas como si fueran válidas con estado parcial.
- [~] Cuota HLS llena, provider lento y FFmpeg fallido tienen errores observables y
  cleanup determinista.

### 15.4 Supply chain

- [x] Imágenes base, snapshot Debian, FFmpeg, Syft, cargo-audit, RustSec, Trivy
  y GitHub Actions tienen pins públicos e inmutables adecuados a cada entrada;
  las descargas de herramientas verifican SHA-256 antes de ejecutarse.
- [x] El QA local 46/46 valida pins, runtime distroless, Compose, auditorías URL/argv, contrato de CI y el registro único de
  excepciones; no hay excepciones activas y cualquier futura aceptación exige
  componente/purl, owner, ticket, motivo y caducidad máxima de 30 días.
- [x] RustSec puede ejecutarse como gate real independiente sin Docker usando
  los mismos pins y excepciones, conservando informe, inputs, estado y
  `SHA256SUMS`; el generador SBOM valida la política incluso en uso standalone.
  La segunda ejecución real confirmó que los dos advisories parcheables ya no
  aparecen y bloqueó únicamente por RUSTSEC-2023-0071 lock-only, sin excepciones.
- [x] CI está configurado para generar SBOM, ejecutar cargo-audit y escanear la
  imagen con Trivy en PR/push/tag y semanalmente, conservando informes y
  findings suprimidos incluso si el gate falla. El schedule no se omite por un
  fallo ajeno; PR/push/tag mantienen sus dependencias obligatorias.
- [x] Jellyfin Web permanece en la base oficial 10.11.11 y aplica de forma
  reproducible el commit oficial de PR #7617 para Swiper 12.1.2. El builder
  verifica ambos SHA-256, limita el patch a los manifests, instala con
  `--omit=optional`, excluye canvas/node-pre-gyp/tar y publica atómicamente. El
  build real produjo 2.317 ficheros, 60 MiB y cero symlinks. El gate E2E
  Playwright pasó 1/1 contra una instancia y base PostgreSQL descartables:
  slideshow con imagen real y lector CBZ de tres páginas, worker, navegación,
  RTL y vista doble, sin respuestas fallidas ni errores de página.
- [x] AArch64 se construyó realmente con Podman rootless; Syft generó SBOM
  SPDX/CycloneDX de imagen/fuente y todos los `SHA256SUMS` verifican. El runtime
  candidato usa distroless fijado por digest, no contiene `curl`, shell ni
  package manager, conserva healthcheck y UID/GID 10001. Su corpus remux pasa y
  la imagen Docker exacta `630a430` pasó runtime smoke, Compose real y los gates
  RustSec/Trivy/NVD. Los manifests de evidencia son
  `1a150e4d...0f1b7` (SBOM) y `7b1fd60e...ee1f5` (vulnerabilidades). Falta
  construir y escanear AMD64 nativo y evitar cualquier excepción sin
  owner/ticket/expiración reales antes de promover.
- [ ] Firmar el digest promovido, adjuntar provenance y comprobar pull y
  ejecución por digest en el registro real.

No se fija un porcentaje universal de CPU: depende de resolución, frame rate,
códec, bitrate y contenido. Los umbrales finales se establecerán con fuentes
reales del proveedor.

## 16. Orden recomendado de implementación

### 16.1 Prerrequisitos comunes

1. `[~]` Ya están fijados el catálogo Xtream real, VOD/direct, Live/remux y las
   métricas de catálogo; faltan clientes Jellyfin reales, MAGSTV y carga
   sostenida para cerrar la baseline.
2. `[~]` El contrato neutral y el grant `ProviderSecrets` del core están
   implementados. El parche que los consume en `jellyrin-plugin-magstv` y retira
   el fallback de credenciales de cuenta por entorno ya está aplicado y validado
   contra el core local con 91/0/4 ignoradas y clippy/fmt/diff verdes. Integra
   `origin/main` `2700d7f` por `43551fe`, la adaptación local `8ce47b4` y 0.1.1
   `9596f1c`; su ZIP AArch64 está validado, instalado y activo en staging.
   Su UI corregida solicita únicamente credenciales de cuenta. Faltan egress,
   secretos operativos, crear el tuner con una cuenta real, publicar ambos,
   fijar el pin público compatible y ejecutar la resolución JIT E2E
   con una cuenta controlada, sin persistir secretos ni URLs firmadas.
3. `[~]` SQLite persistente usa rollback journal fail-closed y no WAL mientras
   siga fijado el bundle 3.46.0; actualizar SQLx/`libsqlite3-sys` junto a Rust
   antes de reactivar WAL.
4. `[~]` DB, sync, delivery, FFmpeg principal/auxiliar y ffprobe ya tienen
   telemetría acotada y sin secretos; `pg_stat_statements` está precargado e
   instalado en la base `jellyrin`, donde registra 49 statements. Faltan
   revisión periódica de sus consultas, vistas operativas y medición productiva.
5. `[x]` Preparar Compose PostgreSQL, CI y secretos y ejecutar Docker Compose
   real sobre la imagen ARM64 exacta del HEAD, sin dejar daemon ni volúmenes
   efímeros activos.

### 16.2 Track PostgreSQL

1. `[x]` Centralizar drivers/factory y cerrar SQLx, SQL y pools fuera de
   `jellyrin-db`; la API ya está limpia.
2. `[x]` Crear baseline PostgreSQL, tipos y constraints; provider refs seguras y
   vault Xtream están implementados y pasan la suite PostgreSQL real.
3. `[x]` Portar repositorios dominio a dominio con tests PostgreSQL.
4. `[~]` Staging/generaciones, batch atómico, no-op e índices están
   implementados. `/Items` tiene pushdown/cap 500 parcial; colecciones/filtros
   simples usan facetas/CTE sin cap y los candidatos por tipo efectivo usan
   prefiltrado sargable. Los filtros Movie/Episode tienen resumen completo y
   reconciliación puntual CAS hasta 119, sin paginación. La proyección facet
   está versionada por 109 y sus
   selectores `GenreIds` por 110 y Upcoming temporal por 111. Quedan predicados complejos y `EXPLAIN`/p95 de handlers
   representativos. COPY ya fue medido a 100k/500k y descartado por una mejora
   de solo 1,046x/1,030x.
5. `[~]` `jellyrin-migrate` pasa 36/36 en PostgreSQL real; `provider_secrets`
   durable y Bytes BLOB↔`bytea` están validados byte a byte. El restore drill
   del estado actual de staging pasó con 49 tablas; falta repetirlo tras la
   importación del snapshot representativo y replicar la evidencia off-host.
6. `[~]` La migración SQLite→PostgreSQL pasa fixtures y PostgreSQL real; ejecutar
   un snapshot representativo solo si aparece una SQLite legacy real. El staging
   actual nació en PostgreSQL y no tiene ese cutover pendiente.
7. `[~]` El cutover SQLite es igualmente condicional. Para el staging actual el
   gate aplicable es backup/restore, migrator→runtime y rollback PostgreSQL, ya
   ensayados; falta replicar el backup fuera del host.
8. `[x]` SQLite está retirado del grafo normal del servidor y reconocido como
   driver real no productivo mediante feature `sqlite`; MySQL queda reservado.

### 16.3 Track FFmpeg

Después de instrumentación, este track puede desarrollarse en paralelo:

1. `[x]` Coordinador central, cupo agregado más semáforos por carril, control de
   hijos, readrate, watchdog, process groups y cierre
   `SIGTERM`/gracia/`SIGKILL` con reap.
2. `[x]` Direct proxy VOD/series y Live TV selectivo.
3. `[x]` Decisión estructurada y remux/transcodificación parcial.
4. `[x]` Bitrate, resolución, profile, level, bit depth, frame rate y channels
   están implementados con razones deterministas y pasan la suite global y el
   clippy estricto; tipar internamente esas razones queda como refactor opcional.
5. `[x]` HLS rolling, seek sin duplicados, cuota y retención.
6. `[x]` Fallback copy-first remux→encode limitado a dos intentos en `enabled`,
   sin fallback por cancelación/idle/cuota y con modo efectivo, fase, código de
   error cerrado y contador visibles en ActiveEncodings/diagnósticos. La
   registry es efímera y acotada; no conserva stderr, argv, URL ni credenciales.
7. `[~]` Ajuste de software con límites de encoder/filtros, probes Xtream
   persistidos/versionados y subtítulos; falta tuning real y generalizar el
   contrato de probes a otros proveedores.
8. `[x]` Observación HLS numérica: CPU/RSS Linux a 2 s, frame/fps/speed/posición
   para VOD/Live/seek, cleanup del sampler y agregados de cardinalidad fija sin
   credenciales. Una sesión Live real acreditó modo remux, fallback false,
   ~15,3 MiB RSS y cleanup; faltan sesiones concurrentes/sostenidas y clientes.
9. `[x]` Ejecuciones FFmpeg auxiliares bajo admisión compartida y telemetría de
   cardinalidad fija con outcomes y duración, sin retener payload sensible.
10. `[x]` Seek HLS con deadline derivado/cap validado, stop/reap, cleanup seguro
    y estados terminales sin fallback tras timeout o cuota.
11. `[x]` Probes locales y remotos comparten el presupuesto multimedia
    process-wide con FFmpeg, con carril propio, cola acotada, timeout y permisos
    RAII; la admisión externa Xtream se conserva sin adquirir dos veces el
    permiso central.
12. **Fuera del alcance de este host:** aceleración hardware solo en futuros
    hosts compatibles; no es gate de esta entrega software-only.

### 16.4 Integración y rollout

1. Desplegar PostgreSQL y estabilizarlo antes de cambiar la política de entrega,
   o viceversa; nunca activar ambos cambios principales en la misma release.
2. Ejecutar carga, caos, seguridad y restore end-to-end.
3. Ajustar recursos con datos del host, documentar dashboards y alertas.
4. Activar cada política FFmpeg con feature flag y cohortes/clientes conocidos.
5. `[x]` Redis fue evaluado y dio no-go; permanece sin desplegar mientras no
   exista un caso multinodo o benchmark nuevo que supere los gates.

Cada fase debe tener migración, métricas, criterio de salida y rollback. Mantener
defaults conservadores hasta que Jellyfin Web, Android TV y los golden tests
confirmen compatibilidad.

## 17. Registro de riesgos

| Riesgo | Prevención | Detección | Recuperación |
| --- | --- | --- | --- |
| IDs media simples/hyphenated cambian el contrato | Normalización UUID con serialización Jellyfin explícita | Golden tests y comparación de endpoints | Revertir capa de presentación, no datos |
| Collation PostgreSQL altera igualdad u orden | Índices `lower`, collation fijada y fixtures multilingües | Tests con acentos/case y diff de respuestas | Ajustar índice/consulta con migración expand/contract |
| Sync elimina progreso o deja catálogo vacío | Staging transaccional por chunks, upsert distinto y publicación multi-biblioteca atómica | Invariantes y sync fault tests | Mantener generación anterior y reintentar |
| Port SQL cambia NULL/cascade/boolean/time | Tipos nativos y tests por repositorio | CI PostgreSQL y auditoría de FKs | Restaurar dump o rollback antes de abrir escrituras |
| Migración tarda demasiado | Dry run con snapshot real y reconstrucción de datos externos | Tiempos por fase y ETA en reporte | Cancelar antes de cutover; SQLite queda intacto |
| PostgreSQL compite con FFmpeg por CPU/RAM | Pools pequeños, cupo FFmpeg agregado, un encode y dos threads para encoder/filtros | CPU, RSS, pool waits y `speed` | Reducir worker/encode; pausar sync durante encode si se mide necesario |
| Redis futuro devuelve caché obsoleta si se reabre el no-go | Invalidación post-commit, versionado y TTL | Comparación muestreada con PostgreSQL | Bump de namespace o flush solo del prefijo Jellyrin |
| Redis futuro bloquea Jellyrin si se reabre la decisión | Timeouts cortos, circuit breaker y cache-aside | Métricas de fallback y health Redis | Bypass de caché; PostgreSQL sigue operativo |
| Cambio direct/remux rompe un cliente | Decision engine, perfiles y feature flags por cliente | Matriz Jellyfin Web/TV y razones | Desactivar modo nuevo para ese perfil |
| FFmpeg queda huérfano o duplica sesión | Coordinador, RAII, dedupe, process groups y cierre graceful con escalado | Reconciliación, tests de grupo y métrica de procesos en staging | Kill/cleanup y reconstrucción del registro |
| HLS llena disco | Rolling window, admisión serializada, reserva RAII, monitor único, cuota y retención | Bytes usados/reservados por sesión y filesystem alerts | Cancelar productor, limpiar sesiones antiguas y aplicar límite físico al volumen |
| URL/credencial se filtra | Provider refs JIT, relay loopback, vault AEAD, XOR Live TV y backfill; reimport legacy obligatorio | Buscar `RemoteSourceUrl`, `RemoteMediaProbe.SourceUrl`, `stream_url`, DB/logs/argv y probar SSRF | Rotar credencial, reimportar catálogo, invalidar cachés y purgar logs afectados |
| Plugin recibe o retiene credenciales fuera de scope | Frontera para todo `ExternalProcess` + `LiveTvProvider`, `ProviderSecrets` doble opt-in, grant ligado a plugin/tuner/acción/revisión, proceso one-shot para toda RPC con grant, lock R/W de lifecycle, detector ampliado y esquema seguro | Matriz cross-repo, canarios de respuesta, inspección de manifest/RPC y E2E sin env credentials | Revocar permiso, detener host, rotar secreto y bloquear la versión del plugin |
| Envelope queda huérfano | El borrado de tuner hace GC exacto y el arranque reconcilia todas las configuraciones de forma serializable/fail-closed; nunca usa `LIKE` | Contadores de reconciliación y tests SQLite/PostgreSQL real con refs compartidas, huérfanas e inválidas | Reparar la configuración inválida que hizo retener candidatos y repetir el arranque |
| Migrador altera o pierde envelopes | Tratar `provider_secrets` como durable, mapping BLOB↔`bytea`, digests y validación de referencias | Fixtures pre/post-vault, comparación byte a byte y FKs lógicas | Abortar antes del cutover, limpiar target y repetir desde backups |
| Writer concurrente publica filtros obsoletos | Revisión monotónica por carpeta, captura inicial, bump CAS único y coverage ligada a revisión | Test con writer externo al advisory, igualdad source/reconciled/coverage y comparación contra fallback | Retirar coverage y rebuild exacto transaccional; endurecer el GUC interno mediante procedimiento/rol estrecho |
| Fallback general materializa un catálogo grande | Cap 500 en contratos SQL y ampliar pushdown por endpoint | RSS, filas candidatas, query count y datasets 10k/100k/500k | Deshabilitar ruta problemática o imponer cap hasta portar su consulta |
| Cambio de contrato rompe MAGSTV | Traits neutrales, revisión fijada y matriz CI cruzada | Compilación y golden tests del plugin | Mantener versión anterior del contrato durante la transición |
| SQLite WAL se corrompe antes del cutover | Actualización inmediata y shutdown para backup | Integrity check preflight | Restaurar backup consistente y repetir migración |

El riesgo de PostgreSQL y el de FFmpeg se desplegarán separados. Un rollback de
aplicación no debe ejecutar automáticamente una migración destructiva de esquema.

## 18. Referencias técnicas

- [Documentación oficial de FFmpeg: `-readrate` y `-re`](https://ffmpeg.org/ffmpeg.html#toc-Advanced-options)
- [Documentación oficial de FFmpeg: progreso y `-stats_period`](https://ffmpeg.org/ffmpeg.html)
- [Documentación oficial del muxer HLS](https://ffmpeg.org/ffmpeg-formats.html#hls-2)
- [Documentación oficial de codecs/libx264](https://ffmpeg.org/ffmpeg-codecs.html)
- [SQLite WAL y aviso WAL-reset](https://www.sqlite.org/wal.html)
- [PostgreSQL: control de concurrencia MVCC](https://www.postgresql.org/docs/current/mvcc.html)
- [PostgreSQL: carga masiva y `COPY`](https://www.postgresql.org/docs/current/populate.html)
- [PostgreSQL: índices](https://www.postgresql.org/docs/current/indexes.html)
- [PostgreSQL: índices GIN y JSONB](https://www.postgresql.org/docs/current/gin.html)
- [PostgreSQL: `pg_trgm`](https://www.postgresql.org/docs/current/pgtrgm.html)
- [PostgreSQL: `EXPLAIN`](https://www.postgresql.org/docs/current/using-explain.html)
- [SQLx PostgreSQL](https://docs.rs/sqlx/0.8.6/sqlx/postgres/)
- [Redis: eviction para cachés](https://redis.io/docs/latest/develop/reference/eviction/)
- [Redis: consideraciones de locks distribuidos](https://redis.io/docs/latest/develop/clients/patterns/distributed-locks/)

La documentación de FFmpeg confirma que `-readrate 1` equivale a lectura en
tiempo real y que `hls_list_size=0` conserva todas las entradas. También advierte
que `split_by_time` puede empeorar algunos reproductores y producir anomalías de
seek, por lo que no debe ser el default.

PostgreSQL documenta `COPY` como la vía de carga masiva y ofrece índices GIN para
JSONB y `pg_trgm` para búsquedas de texto. Jellyrin ya usa `pg_trgm` en índices
de nombre; sus planes deben verificarse sobre datasets reales. `COPY` permanece
como candidato sujeto a benchmark: añade complejidad de staging y manejo de
errores, y más índices también implican más coste de escritura y mantenimiento.

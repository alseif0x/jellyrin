# Decisión sobre Redis para Jellyrin

**Estado (2026-08-08): no integrar ni desplegar Redis.** Se conserva únicamente
scaffolding dormido de Compose bajo profiles explícitos para reproducir el
benchmark o reevaluar una necesidad futura; no es un componente opcional del
despliegue vigente. En la topología objetivo
actual —una instancia de Jellyrin, PostgreSQL local y catálogo multimedia
externo— Redis no elimina trabajo de FFmpeg, no sustituye ningún lock o handle
local y no tiene todavía un consumidor en la aplicación. Activar el contenedor
ahora añadiría memoria, una conexión y un modo de fallo con cero cache hits.

Esta decisión es revisable. La sección [Umbral de activación](#umbral-de-activación)
define qué evidencia tendría que cambiarla.

## Auditoría del estado actual

No existe dependencia de Redis en ningún `Cargo.toml` ni una URL Redis leída por
el servidor. `AppState` contiene PostgreSQL y paths locales, no un cliente de
caché. La definición de `docker-compose.infrastructure.yml` es por tanto solo
scaffolding operativo: profile `cache`/`distributed-cache`, sin puerto publicado,
autenticado, sin AOF/RDB y con `allkeys-lru`.

| Necesidad | Propietario actual | Decisión |
| --- | --- | --- |
| Usuarios, tokens, sesiones de dispositivo, progreso y listas | PostgreSQL | Deben seguir siendo durables; Redis no será fuente primaria. |
| Quick Connect y sesiones activas/transcode | PostgreSQL, con expiración o reconciliación | El estado se necesita después de un reinicio; no moverlo a una caché sin persistencia. |
| Catálogo de paquetes | Tabla materializada `package_catalog_cache` | Ya evita consultar repositorios externos en lectura; medir la consulta antes de duplicarla. |
| Exclusión de sync de catálogo, configuración de plugins y emisión de token | Advisory locks/row locks PostgreSQL | Son suficientes para una o varias instancias y se liberan con transacción o conexión. |
| Cupos FFmpeg y de probes | Semáforos en el proceso propietario | Redis no puede poseer un proceso hijo ni devolver un permiso RAII. |
| Cancelación FFmpeg/recording, broadcast de stream y leases de tuner | Handles y canales locales | Deben quedarse junto al proceso/socket. En multinodo haría falta routing por `node_id`, fencing y estado durable, no solo Redis. |
| SyncPlay y eventos websocket | Estado/canales locales con limpieza de participantes stale | Distribuirlo exige rediseñar ownership y replay. Pub/Sub no es adecuado para órdenes críticas porque su entrega es como máximo una vez. |
| Lockout de login | `AUTH_FAILURES` en memoria | En single-node no necesita una ida de red. Antes hay que acotar y expirar el mapa; en multinodo se reevaluará un rate limit compartido. |

PostgreSQL ya ofrece el componente de coordinación compartida necesario. Sus
advisory locks tienen semántica de sesión o transacción y los de sesión se
liberan cuando termina la conexión. Redis sigue siendo válido en el futuro para
cache-aside o invalidaciones best-effort, pero no mejora las garantías de los
casos anteriores.

### Hallazgo previo a cualquier caché distribuida

La inspección estática encontró mapas locales de single-flight cuya entrada no
se elimina de forma visible después del trabajo:

- `TRANSCODE_DEDUPE_LOCKS`, una clave por decisión/sesión de transcode;
- `TRICKPLAY_TILE_LOCKS`, una clave por tile generado;
- `XTREAM_PROBE_LOCKS`, una clave por item remoto abierto;
- fallos de autenticación que no alcanzan lockout, que permanecen hasta un login
  correcto de esa misma clave.

Esto no demuestra por sí solo una fuga observable, pero su cardinalidad puede
crecer con el catálogo o con nombres de usuario controlados por un atacante. Es
prioritario instrumentar el número de entradas y cambiar los locks por entradas
removibles/`Weak` o una caché local con TTL y límite. Moverlos a Redis solo
cambiaría dónde crece el estado y añadiría latencia de red; tampoco serviría para
los guards que protegen handles locales.

## Benchmark reproducible

El runner aislado `qa/redis-cache-benchmark.sh` arranca Redis únicamente en
`127.0.0.1`, con password efímero, persistencia deshabilitada y el mismo límite
de 64 MiB del profile. Precarga 50.000 valores de 1 KiB y mide lecturas sin
pipeline con seis clientes, igual que el máximo inicial del pool API de
PostgreSQL. Si se proporciona un DSN de pruebas, crea y elimina un schema
PostgreSQL aislado con el mismo dataset:

```bash
JELLYRIN_REDIS_EVAL_POSTGRES_URL='postgresql://usuario:secreto@127.0.0.1/base_pruebas' \
  ./qa/redis-cache-benchmark.sh
```

La comprobación de presión de memoria/eviction es opcional:

```bash
JELLYRIN_REDIS_EVAL_SATURATE=1 ./qa/redis-cache-benchmark.sh
```

El 2026-08-08 se ejecutó en ARM64, cuatro CPU, Redis 7.0.15 y PostgreSQL
16.14. El host tenía carga concurrente, por lo que estos números son una foto
reproducible y no una capacidad contractual. Compose fija Redis 7.2.14 y añade
red de contenedor; el test TCP loopback es una aproximación favorable para
Redis. Ambos datasets estaban calientes.

| Medida | Resultado |
| --- | ---: |
| RSS Redis vacío | 12.080 KiB |
| RSS Redis con ~50k × 1 KiB | 79.888 KiB |
| `used_memory` Redis cargado | 69.595.552 bytes |
| Redis GET, 6 clientes, sin pipeline | 37.665 req/s |
| Redis GET promedio / p95 / p99 | 0,110 / 0,199 / 0,783 ms |
| Tamaño total de la relación PostgreSQL | 59.695.104 bytes |
| PostgreSQL SELECT preparado, 6 clientes | 37.757 tx/s |
| PostgreSQL transacción / statement promedio | 0,159 / 0,119 ms |

En otra pasada hasta `maxmemory=96 MiB`, Redis alcanzó aproximadamente 107 MiB
de RSS, expulsó más de 384.000 claves durante la carga y la lectura uniforme
posterior obtuvo alrededor de 71,5 % de hits. El límite del contenedor es
128 MiB: quedan solo unos 21 MiB observados para fragmentación, buffers de
clientes y picos. Si algún día se activa el profile, debe comenzar con un
working set menor (32–64 MiB), `maxclients` acotado y volver a medirse dentro
del cgroup real.

El benchmark demuestra que Redis es rápido, no que Jellyrin lo necesite. En esta
carga, PostgreSQL caliente ya sostuvo decenas de miles de lecturas por segundo y
la diferencia media por lookup fue menor de una décima de milisegundo. La API,
serialización JSON y red del proveedor dominan mucho antes. Además, la copia de
50 MiB ocupó cerca de 68 MiB adicionales dentro de Redis; PostgreSQL y su page
cache ya forman parte del presupuesto obligatorio.

## Decisión operativa

1. No añadir por ahora crate/cliente Redis, `REDIS_URL`, health/readiness ni
   invalidaciones en la aplicación.
2. Mantener `redis` solo como scaffolding dormido bajo profiles explícitos y no
   iniciarlo en instalación, upgrade ni arranque normal.
3. Mantener locks de sync en PostgreSQL y handles/canales/semaforización junto
   al proceso que posee FFmpeg o el stream.
4. Optimizar primero consultas e índices con `EXPLAIN (ANALYZE, BUFFERS)`, y
   acotar los mapas locales hallados. Una caché no debe ocultar una consulta o
   una cardinalidad defectuosa.
5. No almacenar tokens, credenciales, URLs de proveedor, segmentos, imágenes ni
   payloads grandes en Redis.

## Umbral de activación

Redis solo pasa de scaffolding a implementación si existe un caso concreto y se
cumplen **todos** los criterios siguientes:

1. **Causa demostrada:** hay al menos dos nodos que necesitan rate limit o
   invalidación compartida, o un endpoint single-node incumple su SLO y
   `pg_stat_statements`/tracing atribuye al menos 30 % de su tiempo o CPU a una
   lectura PostgreSQL repetida. Un `EXPLAIN` correcto y los índices deben estar
   ya aplicados.
2. **Valor material:** una prueba A/B end-to-end con la carga objetivo mejora el
   p95 del endpoint al menos 25 % y 10 ms, o reduce al menos 30 % la CPU/lecturas
   PostgreSQL, sin aumentar errores. La mejora de microsegundos de un GET aislado
   no basta.
3. **Reutilización real:** hit ratio estable de al menos 80 % después de warmup;
   evictions menores al 1 % de comandos; ningún valor mayor de 64 KiB; TTL corto
   con jitter y working set medido, no estimado.
4. **Presupuesto:** RSS Redis menor al 5 % de la RAM del host y quedan al menos
   512 MiB libres durante un encode FFmpeg de peor caso. En este host se empieza
   con `maxmemory` de 32–64 MiB, no 96 MiB, y se dimensiona el cgroup con RSS
   saturado, no con `used_memory` vacío.
5. **Degradación segura:** timeout corto, circuit breaker y fallback a
   PostgreSQL mantienen el SLO cuando Redis está detenido o saturado. Redis no
   participa en `/readyz` si solo es caché y su caída no impide login, browse ni
   playback.
6. **Correctitud:** claves versionadas y hasheadas, invalidación solo después de
   commit, comparación muestreada cache/DB y prueba de datos stale. No se
   permiten secretos ni identificadores de alta sensibilidad en claves o
   valores.

El primer candidato razonable sería rate limit compartido al desplegar dos o
más nodos. Para catálogos, se escogerá una única respuesta cara e inmutable por
TTL; no se añadirá una caché genérica de repositorio. Pub/Sub solo podrá llevar
invalidaciones descartables: la documentación de Redis especifica entrega
at-most-once, por lo que cancelaciones y órdenes críticas necesitan estado
durable y replay.

## Referencias

- [Redis: límites de memoria y eviction](https://redis.io/docs/latest/develop/reference/eviction/)
- [Redis: semántica de Pub/Sub](https://redis.io/docs/latest/develop/pubsub/)
- [PostgreSQL: advisory locks](https://www.postgresql.org/docs/current/explicit-locking.html#ADVISORY-LOCKS)

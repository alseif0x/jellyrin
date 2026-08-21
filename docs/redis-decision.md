# Decisión sobre Redis para Jellyrin

**Estado (revalidado el 2026-08-21): Redis es un caché opcional y fail-open para
facetas públicas compartidas del catálogo.** La decisión cambió al confirmar que
la instalación servirá a muchos usuarios y que géneros, estudios, personas,
etiquetas y años repiten exactamente la misma proyección por biblioteca. No se
ha convertido Redis en fuente de verdad ni en requisito de disponibilidad.

PostgreSQL sigue resolviendo usuarios, permisos, asignación de cuenta de plugin,
progreso, favoritos, sesiones, límites de dispositivos y todo dato de playback.
Redis tampoco sustituye locks, semáforos, handles FFmpeg ni canales locales.

## Auditoría del estado actual

El servidor usa `redis-rs` con una conexión async multiplexada y reconexión. La
configuración es opcional mediante `JELLYRIN_REDIS_URL` o, preferiblemente con
systemd, `JELLYRIN_REDIS_URL_FILE`. Compose conserva el profile explícito
`cache`/`distributed-cache`, sin puerto publicado, autenticado, sin AOF/RDB y
con `allkeys-lru`.

El consumidor está deliberadamente acotado:

- solo guarda vectores de nombres públicos de facetas;
- la clave incluye namespace versionado, tipo de faceta y SHA-256 del conjunto
  ordenado de bibliotecas; no expone UUID en claro;
- cada valor tiene un máximo de 64 KiB y TTL configurable de 5–300 s (30 s por
  defecto);
- cada comando tiene timeout de 5–250 ms (20 ms por defecto);
- un error abre un bypass de cinco segundos: durante esa ventana se consulta
  PostgreSQL directamente;
- un single-flight local evita que una expiración lance la misma consulta muchas
  veces desde un proceso;
- Redis no participa en `/readyz` y su caída no bloquea arranque, login,
  navegación ni reproducción.

| Necesidad | Propietario actual | Decisión |
| --- | --- | --- |
| Usuarios, tokens, sesiones de dispositivo, progreso y listas | PostgreSQL | Deben seguir siendo durables; Redis no será fuente primaria. |
| Quick Connect y sesiones activas/transcode | PostgreSQL, con expiración o reconciliación | El estado se necesita después de un reinicio; no moverlo a una caché sin persistencia. |
| Facetas públicas del catálogo | PostgreSQL + Redis cache-aside opcional | Compartibles entre usuarios; TTL corto, valor acotado y fallback transparente. |
| Catálogo de paquetes | Tabla materializada `package_catalog_cache` | Ya evita consultar repositorios externos en lectura; no duplicarlo en Redis. |
| Exclusión de sync de catálogo, configuración de plugins y emisión de token | Advisory locks/row locks PostgreSQL | Son suficientes para una o varias instancias y se liberan con transacción o conexión. |
| Cupos FFmpeg y de probes | Semáforos en el proceso propietario | Redis no puede poseer un proceso hijo ni devolver un permiso RAII. |
| Cancelación FFmpeg/recording, broadcast de stream y leases de tuner | Handles y canales locales | Deben quedarse junto al proceso/socket. En multinodo haría falta routing por `node_id`, fencing y estado durable, no solo Redis. |
| SyncPlay y eventos websocket | Estado/canales locales con limpieza de participantes stale | Distribuirlo exige rediseñar ownership y replay. Pub/Sub no es adecuado para órdenes críticas porque su entrega es como máximo una vez. |
| Lockout de login | `AUTH_FAILURES` en memoria | En single-node no necesita una ida de red. Antes hay que acotar y expirar el mapa; en multinodo se reevaluará un rate limit compartido. |

PostgreSQL ofrece el componente de coordinación compartida necesario. Sus
advisory locks tienen semántica de sesión o transacción y los de sesión se
liberan cuando termina la conexión. Redis solo reduce lecturas repetidas y no
mejora las garantías de los casos anteriores.

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
reproducible y no una capacidad contractual. Compose fija Redis 8.2.8 y añade
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

### Revalidación con el catálogo real (2026-08-21)

La auditoría de Home y bibliotecas confirmó que Redis no debía ocultar planes defectuosos. `Latest`
materializaba cerca de un millón de filas y `NextUp` enviaba 43.363 candidatos a Rust para mostrar
20. Tras paginar en PostgreSQL y respetar `EnableTotalRecordCount`, el SQL de `NextUp` sin total
promedia 0,407 ms y el endpoint completo 31–54 ms. El conteo exacto posterior bajó su plan de unos
466 ms a unos 12 ms. Esos caminos dependientes del usuario no entran en Redis. La activación se
limita a facetas compartidas, donde muchos usuarios reutilizan el mismo resultado.

El rollout real se hizo con Redis 8.2.8, autenticado, limitado a loopback, `maxmemory=64 MiB`
y cgroup de 96 MiB. Tras vaciar exclusivamente la caché efímera, la faceta de géneros de
Películas (56 valores, respuesta de 36.676 bytes) tardó 185 ms en frío y 6–8 ms en cinco
lecturas calientes. Redis confirmó cinco hits, dos misses del fill con recheck, cero errores y
un único valor con TTL; `used_memory` fue aproximadamente 645 KiB. Las seis respuestas fueron
idénticas byte a byte.

La prueba fail-open detuvo deliberadamente solo Redis: la misma ruta siguió respondiendo 200
desde PostgreSQL en 205 ms y Jellyrin permaneció activo. Tras restaurarlo y superar el bypass de
cinco segundos, la primera lectura repobló la clave en 186 ms y la siguiente volvió a 6 ms, con
contenido idéntico. El contenedor terminó `healthy`, sin reinicios ni OOM. Esta mejora de unas
25–30 veces en caliente sí supera el umbral de activación para una proyección compartida por
muchos usuarios; no justifica cachear respuestas personalizadas.

Los resultados completos, incluidos imágenes, gzip e índices, están en
[`catalog-performance.md`](catalog-performance.md).

## Decisión operativa

1. Redis sigue siendo opcional; no configurar URL equivale exactamente al camino
   PostgreSQL anterior.
2. Activarlo solo bajo profile o unidad explícitos, autenticado y sin publicar
   el puerto fuera de loopback/red backend.
3. Mantener locks de sync en PostgreSQL y handles/canales/semaforización junto
   al proceso que posee FFmpeg o el stream.
4. Optimizar primero consultas e índices con `EXPLAIN (ANALYZE, BUFFERS)`; una
   caché no debe ocultar una consulta o cardinalidad defectuosa.
5. No almacenar tokens, credenciales, URLs de proveedor, segmentos, imágenes ni
   payloads grandes en Redis.
6. Medir `keyspace_hits`, `keyspace_misses`, evictions, RSS y latencia p95 antes
   de ampliar el conjunto de claves.

### Activación segura

Para Compose completo, definir `REDIS_PASSWORD`, habilitar el profile `cache` y
proporcionar `JELLYRIN_REDIS_URL` en el fichero de entorno protegido. En un
despliegue bare-metal de Jellyrin con Redis aislado en Docker se usa
`ops/docker-compose.redis-cache.yml`: recibe un fichero root-only con la
contraseña, deriva un ACL SHA-256 solo dentro del tmpfs
del contenedor, publica únicamente `127.0.0.1:16379` y mantiene 64 MiB de
`maxmemory` dentro de un cgroup de 96 MiB. La contraseña no aparece en argv,
variables Docker ni `docker inspect`.

La URL de Jellyrin se guarda aparte, por ejemplo
`redis://:CONTRASEÑA@127.0.0.1:16379/0`, en un fichero 0400/0440 fuera del repo.
El drop-in `ops/jellyrin-redis-cache.conf.example` la entrega mediante
`LoadCredential`; no se debe copiar la URL a logs, commits o argumentos de
proceso. Tras activar:

1. comprobar health del contenedor y que el listener sea solo loopback;
2. reiniciar Jellyrin y confirmar el evento fijo `shared Redis catalogue cache enabled`;
3. ejecutar dos lecturas idénticas de Géneros/Estudios y verificar miss→hit;
4. detener Redis temporalmente y confirmar que la misma ruta sigue respondiendo
   desde PostgreSQL dentro del SLO;
5. restaurar Redis y comprobar reconexión antes de declarar el rollout completo.

## Umbral de activación

La faceta compartida cumple el umbral funcional inicial. Cualquier ampliación a
otro tipo de dato debe cumplir **todos** los criterios siguientes:

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

No se añadirá una caché genérica de repositorio. Pub/Sub solo podrá llevar
invalidaciones descartables: la documentación de Redis especifica entrega
at-most-once, por lo que cancelaciones y órdenes críticas necesitan estado
durable y replay.

## Referencias

- [Redis: límites de memoria y eviction](https://redis.io/docs/latest/develop/reference/eviction/)
- [Redis: patrón cache-aside](https://redis.io/docs/latest/develop/use-cases/cache-aside/)
- [Redis: semántica de Pub/Sub](https://redis.io/docs/latest/develop/pubsub/)
- [PostgreSQL: advisory locks](https://www.postgresql.org/docs/current/explicit-locking.html#ADVISORY-LOCKS)

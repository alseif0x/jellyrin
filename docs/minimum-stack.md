# Stack mínimo de despliegue

Este documento separa los componentes imprescindibles de los opcionales y evita convertir el
perfil de un servidor concreto en un requisito universal. Los límites de Compose son presupuestos
del contenedor, no reservas permanentes de memoria o CPU.

## Componentes

| Componente | Estado | Motivo |
| --- | --- | --- |
| Jellyrin y `jellyrin-migrate` | Obligatorio | El migrador aplica el esquema antes de arrancar la API. |
| PostgreSQL 17 | Obligatorio en producción | Es la fuente durable de usuarios, catálogo, plugins, progreso y sesiones. No se publica su puerto. |
| FFmpeg/ffprobe | Obligatorio en la imagen | Permite probe, remux y transcode cuando el cliente no admite el medio original. |
| Jellyfin Web | Obligatorio si se usa la interfaz web | Se construye con `ops/build-jellyfin-web.sh`; las aplicaciones nativas solo necesitan la API. |
| Redis | Opcional | Caché fail-open para facetas públicas y conteos compartidos de bibliotecas. PostgreSQL sigue respondiendo si Redis cae. |
| nginx u otro proxy inverso | Opcional | Solo hace falta normalmente para TLS, dominio público, límites perimetrales o integración ACME. Jellyrin puede exponerse directamente por su puerto y comprime JSON/web de forma nativa. |
| Sidecars de plugins | Condicional | Solo para plugins cuyo manifiesto y permisos requieran un proceso auxiliar, como el egress aislado de MAGSTV. |
| DLNA override | Opcional | Necesario únicamente cuando se quiere descubrimiento SSDP/UPnP en la LAN. |

PostgreSQL y Redis deben permanecer en la red privada de Compose. Para acceso directo en una LAN se
puede publicar Jellyrin con `JELLYRIN_PUBLISH_ADDRESS=0.0.0.0` y limitar el puerto mediante el
firewall. Para Internet se recomienda terminar TLS en un proxy inverso o túnel seguro; nginx no es
un requisito funcional.

## Recursos

| Perfil | CPU host | RAM host | Disco SSD libre inicial | Uso previsto |
| --- | ---: | ---: | ---: | --- |
| Mínimo funcional | 2 núcleos | 4 GiB | 20 GiB más transcodes | Pruebas, pocos usuarios y catálogos pequeños; Redis desactivado. |
| Catálogo remoto grande | 4 núcleos | 8 GiB | 30 GiB más transcodes/arte | Cientos de miles de items, varios usuarios y Redis opcional. |
| Perfil medido actual | 8 núcleos | 12 GiB | 40 GiB más transcodes/arte | Cerca de un millón de items, MAGSTV/Xtream y clientes Android TV. |

El espacio de transcode depende del bitrate y concurrencia. Debe conservarse el límite del volumen
y `JELLYRIN_TRANSCODE_RESERVATION_BYTES`; el espacio de la tabla no sustituye esa medición.

### Perfil PostgreSQL medido en el host de 12 GiB

El Compose conserva por defecto valores portables de 512 MiB y 0,75 CPU. Este host los amplía desde
su `.env` sin cambiar los requisitos mínimos del proyecto:

| Variable | Valor aplicado |
| --- | ---: |
| `POSTGRES_MEMORY_LIMIT` | `2g` |
| `POSTGRES_CPU_LIMIT` | `2.0` |
| `POSTGRES_SHM_SIZE` | `512mb` |
| `POSTGRES_SHARED_BUFFERS` | `512MB` |
| `POSTGRES_EFFECTIVE_CACHE_SIZE` | `1536MB` |
| `POSTGRES_WORK_MEM` | `8MB` |
| `POSTGRES_MAINTENANCE_WORK_MEM` | `256MB` |
| `POSTGRES_RANDOM_PAGE_COST` | `1.5` |
| `POSTGRES_EFFECTIVE_IO_CONCURRENCY` | `100` |
| `POSTGRES_TRACK_IO_TIMING` | `on` |

No se deben aumentar a la vez `work_mem`, el pool y la concurrencia de workers sin medir. Una
consulta puede usar varios nodos de sort/hash y multiplicar `work_mem`. El punto inicial de la API
sigue siendo seis conexiones interactivas y dos de worker.

### Redis

Para catálogos grandes, el perfil medido usa 128 MiB de datos dentro de un contenedor de 192 MiB,
`allkeys-lru`, 64 clientes, persistencia desactivada y autenticación. Solo guarda proyecciones
regenerables, nunca tokens, credenciales, URLs firmadas, progreso o asignaciones de usuario.
El despliegue Compose debe añadir `docker-compose.redis-cache.yml`, definir
`JELLYRIN_REDIS_URL_HOST_FILE` con un fichero `root:10001` modo `0440` y activar el profile
`cache`/`distributed-cache`. Sin ese overlay, Redis puede ejecutarse pero Jellyrin no lo consume.

## Software y arquitecturas

- Linux `x86_64` o `aarch64` con Docker Engine y Docker Compose v2.
- Un kernel con cgroups v2 y almacenamiento SSD para PostgreSQL es lo recomendado.
- Rust, Node.js y npm solo son necesarios para compilar desde fuente y ejecutar QA; la imagen de
  runtime ya contiene las dependencias necesarias.
- Los artefactos e imágenes deben conservar los hashes fijados en `ops/supply-chain.lock.env`.

## Validación después de dimensionar

1. Confirmar `/health` y `/readyz`.
2. Medir frío/caliente sin resetear `pg_stat_statements` durante la observación normal.
3. Revisar throttling de cgroup, `memory.events`, temporales y timeouts PostgreSQL.
4. Comprobar `keyspace_hits`, misses y evictions de Redis si está habilitado.
5. Verificar catálogos, imágenes, VOD, Live TV, seek, audio y subtítulos desde el cliente real.

Las decisiones y umbrales detallados de caché están en [redis-decision.md](redis-decision.md), y
las mediciones del catálogo en [catalog-performance.md](catalog-performance.md).

# Drivers de base de datos

Jellyrin tiene ahora un punto único de selección y construcción de la base de datos, parecido al
`DatabaseManager` de frameworks como Laravel, pero conserva una diferencia importante: cada
adaptador usa SQL, tipos y migraciones nativos. No se usa `AnyPool` ni se rebajan las consultas al
mínimo común entre dialectos.

## Estado real de soporte

| Selector | URL | Estado | Uso permitido |
| --- | --- | --- | --- |
| `postgresql` (también `postgres`) | `postgresql://...` o `postgres://...` | Producción | Runtime, migraciones y tests de integración |
| `mysql` | `mysql://...` | Planificado, no implementado | El nombre se reconoce, pero el manager falla antes de abrir una conexión |
| `sqlite` | `sqlite:...` | Adaptador real, aún no productivo | Tests y migración de datos; la factory lo reconoce y el manager de producción lo rechaza explícitamente |

Reconocer `mysql` no significa que Jellyrin sea compatible con MySQL. Sirve para devolver un error
claro y estable mientras se implementa un adaptador completo. PostgreSQL sigue siendo el único
driver `production-ready`.

`sqlite` es el selector público canónico. `sqlite-legacy` se conserva únicamente como alias
compatible y deprecado para configuraciones antiguas; no debe aparecer en documentación ni
despliegues nuevos. El alias del feature de Cargo se llama, por separado, `legacy-sqlite`; ambos
normalizan al driver canónico `sqlite` y ninguno habilita soporte productivo.

SQLite es, por tanto, un driver explícito de primera clase en la frontera arquitectónica, no un
fallback oculto ni una colección de consultas sueltas. Su estado no productivo describe la matriz
de conformidad pendiente, no la ausencia de un adaptador.

## Límite de arquitectura

El flujo de arranque es:

```text
JELLYRIN_DB_DRIVER + DATABASE_URL + límites/timeouts
                         |
                    DatabaseConfig
                         |
                   DatabaseManager
            |            |            |
            |            |            `--> MySQL: selector reservado, sin adaptador
            |            `--------------> SQLite: harness/migración, no runtime
            `---------------------------> PostgreSQL: repositorios productivos
```

- `crates/jellyrin-db/src/driver.rs` identifica drivers y valida que el selector coincida con el
  esquema de la URL sin incluir credenciales en los errores.
- `crates/jellyrin-db/src/manager.rs` contiene `DatabaseConfig` y la factory central
  `DatabaseManager`. Es el único lugar del runtime que decide qué adaptador construir.
- `crates/jellyrin-db/src/postgres*.rs` contiene la implementación y SQL PostgreSQL nativos.
- `crates/jellyrin-db/migrations-postgres/` contiene exclusivamente el esquema PostgreSQL.
- La API consume operaciones de dominio; no debe seleccionar drivers ni ramificar por dialecto.

El alias `ProductionDatabase` es hoy `PostgresDatabase`. Cuando exista un segundo adaptador, ese
frente deberá evolucionar junto con todos los contratos de repositorio; no basta con añadir una
rama al manager.

Por tanto, que un selector sea reconocido y que un adaptador sea construible en producción son
capacidades distintas. `sqlite` y `mysql` se validan como nombres conocidos para evitar fallbacks
silenciosos, pero solo `postgresql` tiene hoy `is_production_supported() == true`.

## Configuración

El servidor acepta estas variables:

| Variable | Predeterminado | Validación |
| --- | ---: | --- |
| `JELLYRIN_DB_DRIVER` | `postgresql` | PostgreSQL es el único disponible en producción |
| `DATABASE_URL` | requerido | Su esquema debe coincidir con el driver |
| `JELLYRIN_DB_MAX_CONNECTIONS` | `6` | `1..=64` para el pool API |
| `JELLYRIN_DB_WORKER_MAX_CONNECTIONS` | `2` | `1..=16` para importaciones y trabajos |
| `JELLYRIN_DB_ACQUIRE_TIMEOUT_SECONDS` | `5` | Mayor que cero, máximo 60 s |
| `JELLYRIN_DB_IDLE_TIMEOUT_SECONDS` | `600` | Mayor que cero, máximo 1 h |
| `JELLYRIN_DB_MAX_LIFETIME_SECONDS` | `1800` | Mayor que cero, máximo 24 h |
| `JELLYRIN_DB_API_STATEMENT_TIMEOUT_SECONDS` | `10` | Mayor que cero, máximo 60 s |
| `JELLYRIN_DB_WORKER_STATEMENT_TIMEOUT_SECONDS` | `120` | Mayor que cero, máximo 30 min |
| `JELLYRIN_DB_LOCK_TIMEOUT_SECONDS` | `3` | Mayor que cero, máximo 60 s |

Ejemplo:

```bash
JELLYRIN_DB_DRIVER=postgresql \
DATABASE_URL='postgresql://jellyrin_runtime:password@127.0.0.1/jellyrin' \
cargo run -p jellyrin-server -- --web-dir ./web
```

`DatabaseConfig` mantiene la URL privada y muestra `[REDACTED]` en `Debug`. La validación
driver-esquema y los errores de configuración omiten la URL completa. En producción, las
credenciales deben llegar mediante un fichero de entorno protegido o un gestor de secretos, no
como argumento visible del proceso. Si PostgreSQL está fuera de una red privada, la URL debe
configurar el modo TLS apropiado.

Los pools API y worker son deliberadamente independientes. Una sincronización larga no puede
consumir todas las conexiones necesarias para login, navegación o heartbeat. PostgreSQL traduce
la configuración común a `PgPoolOptions`, configura `application_name`, UTC, `statement_timeout`
y `lock_timeout` por conexión.

## Diagnósticos neutrales y seguros

Cada adaptador implementa dos superficies sin SQL ni tipos de pool fuera de `jellyrin-db`:

- `runtime_diagnostics()` devuelve driver y contadores `max/size/idle/in_use` de los pools API y
  worker; SQLite indica expresamente que no tiene pool worker independiente.
- `catalog_sync_diagnostics()` devuelve totales por estado y el último run con estado, número de
  items, timestamps y duración.

La ruta administrativa `/System/Diagnostics` publica esos snapshots y limita el health check a un
segundo. No publica URL de conexión, host, usuario, esquema, SQL, IDs de provider/generación ni el
mensaje de error de un sync, porque esos campos pueden contener credenciales. Un adaptador futuro
debe conservar este contrato de seguridad además del contrato funcional.

## Contratos y SQL nativo

La portabilidad se hace en el nivel de dominio, no en el nivel SQL. Por ejemplo,
`MediaCatalogStore` expresa paginación, recuento y estado de reproducción, mientras cada adaptador
implementa esas garantías con sus propias consultas. `XtreamCatalogStore` hace lo mismo para la
indexación externa.

Esto permite usar capacidades PostgreSQL como `UUID`, `JSONB`, `TIMESTAMPTZ`, `pg_trgm`,
transacciones y sus índices específicos. Un futuro MySQL deberá tener consultas, modelos de fila,
índices y migraciones MySQL propios. No se compartirán strings SQL con condicionales por dialecto
ni se introducirán conversiones implícitas para hacer coincidir comportamientos distintos.

Todavía existen operaciones de base de datos como métodos inherentes de los adaptadores. Por eso
la frontera es una base extensible, no una promesa de intercambio instantáneo. Esas operaciones se
extraerán progresivamente a contratos pequeños por dominio antes de habilitar otro driver.

## Cómo añadir MySQL u otro adaptador

Un nuevo driver solo puede pasar a producción cuando complete todos estos pasos:

1. Implementar un adaptador nativo (`mysql.rs` y módulos por dominio), sin `AnyPool`.
2. Crear un árbol de migraciones exclusivo, por ejemplo `migrations-mysql/`, incluida la
   estrategia de upgrade y rollback compatible.
3. Implementar todos los contratos usados por el runtime, no solo login o catálogo.
4. Mapear tipos y errores con semántica equivalente: UUID, fechas UTC, JSON, constraints,
   conflictos unique/FK, timeout, not-found y transacciones.
5. Ejecutar la misma suite de conformidad para PostgreSQL y el nuevo adaptador.
6. Añadir pruebas de migración desde una versión anterior y pruebas de concurrencia/aislamiento.
7. Solo entonces marcar el driver como `production-ready`, incorporarlo a la factory y publicar
   imágenes/configuración soportadas.

El selector SQLite es parte pública de la frontera de drivers y la factory lo reconoce. Su
adaptador `SqliteDatabase` es real, pero permanece detrás del feature canónico `sqlite` para tests
rápidos y migración histórica. `legacy-sqlite` sigue existiendo solo como alias de compilación
compatible. No es la implementación de referencia SQL, un runtime soportado en producción ni una
ruta de fallback si PostgreSQL falla.

Mientras SQLx 0.8 mantenga fijado el SQLite embebido anterior al fix WAL-reset, las bases SQLite
persistentes usan rollback journal y no WAL. Es una mitigación temporal comprobada por test, no
una razón para devolver SQLite al servidor; reactivar WAL exige actualizar y verificar primero la
cadena SQLx/`libsqlite3-sys`.

## Suite de conformidad

Cada contrato de repositorio debe exponer escenarios reutilizables que reciban una implementación
del trait. La matriz objetivo es:

```text
contrato de dominio
  |- PostgreSQL: obligatorio, CI normal + integración con servidor real
  |- MySQL: obligatorio antes de habilitar el selector
  `- SQLite: tests rápidos cuando su semántica sea representable
```

La suite debe comprobar resultados y garantías observables: totales exactos con paginación,
orden estable, filtros case-insensitive, idempotencia, preservación de estado, conflictos,
rollback, aislamiento y límites de timeout. Además habrá smoke tests separados para migraciones y
salud del esquema; pasar tests unitarios sin ejecutar las migraciones nativas no acredita un
driver.

La prueba `postgres_api_and_worker_pools_remain_isolated_when_saturated` mantiene un `pg_sleep`
confirmado por PID en un pool de tamaño uno y exige que el otro responda en menos de 500 ms; se
ejecuta en ambos sentidos. Para obtener una muestra local comparable sin convertir el benchmark en
un test de CI frágil:

```bash
JELLYRIN_TEST_POSTGRES_URL='postgresql://...' \
cargo test -p jellyrin-db postgres_pool_local_load \
  -- --ignored --nocapture --test-threads=1
```

El runner emite JSON con p50/p95/p99, throughput, errores y la razón p95 entre baseline y worker
saturado; no imprime la URL ni los parámetros de conexión.

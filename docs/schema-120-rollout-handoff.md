# Handoff: rollout del esquema 120

Fecha de corte: 2026-08-10 22:36 UTC.

## Estado exacto al cerrar esta sesión

- Commit preparado: `c89ccd8 security: protect query-filter summary publication`.
- Rama local: `main`, 52 commits por delante de `origin/main`.
- Jellyrin está **detenido intencionadamente** (`inactive/dead`, resultado
  anterior `success`, `NRestarts=0`). PostgreSQL sigue activo.
- La base productiva continúa en `202608080119`; la migración 120 todavía no se
  ha aplicado.
- Los binarios instalados siguen siendo los anteriores:
  - servidor: `17667c0d25bcfac639dd720c01fb0adae259376b987ed75527e40ad13f0c5039`;
  - migrador: `2e161e19b7f4e614371e48e7ea2b444678817ab7576b8a4a348938bd0738f5e2`.
- Los artefactos release nuevos están listos:
  - `target/release/jellyrin-server`:
    `f4124471cd525245aca9ada6bab031fccf190c80a144d3937b8aa1a9465997fd`;
  - `target/release/jellyrin-migrate`:
    `954ab2743543900af6e0d4e41bc8d2007cfa256370b403d4c50ffd3495ad7810`.
- Copias recuperables de los binarios anteriores:
  - `/var/backups/jellyrin/jellyrin-server-pre-c89ccd8-20260810T223541Z`;
  - `/var/backups/jellyrin/jellyrin-migrate-pre-c89ccd8-20260810T223541Z`.
- Snapshot PostgreSQL cifrado pre-120 completado correctamente:
  `/var/backups/jellyrin-postgres/daily/20260810T223557Z`.
- No se instalaron binarios nuevos ni se modificó el esquema antes del corte.

Si el rollout no se reanuda inmediatamente, se puede arrancar temporalmente el
binario anterior porque la base todavía está en 119. Si se aplica 120, no se
debe arrancar después el servidor antiguo: su writer esperaba DML directo sobre
el resumen que 120 revoca.

## Evidencia ya cerrada

- `cargo +1.94 fmt --all -- --check`: verde.
- Clippy estricto para DB, migrador y API, todos los targets: verde.
- `git diff --check`: verde.
- `jellyrin-migrate`: 37/37 (33 librería + 4 CLI) contra PostgreSQL real.
- `jellyrin-db`: 168 aprobadas y 4 ignoradas en el pase paralelo; el único test
  afectado por un deadlock de la propia suite pasó aisladamente, dejando 169
  pruebas efectivamente verdes.
- Test focal PostgreSQL del resumen: verde.
- API fuera del sandbox y con `/usr/bin/ffmpeg`: 354 aprobadas, 0 fallidas y 3
  ignoradas.
- PostgreSQL 16 aislado: rebuild completo, reconciliación puntual, cambio
  `Drama`→`Action`, subtítulos, rechazo de proyección anterior falsa, ACL de
  solo lectura y spoofing de ambos GUC históricos/sombras temporales: verde y
  fail-closed.
- Build release conjunto de servidor y migrador terminado correctamente.

## Plan de continuación

### 1. Revalidar el punto de partida

Antes de escribir nada:

```bash
git status -sb
git log -1 --oneline
sudo systemctl show jellyrin.service -p ActiveState -p SubState -p NRestarts
sudo -u postgres psql -d jellyrin -Atc \
  "SELECT max(version) FROM _sqlx_migrations WHERE success"
sha256sum target/release/jellyrin-server target/release/jellyrin-migrate
```

Exigir `c89ccd8`, servicio detenido, esquema 119 y los hashes anotados arriba.
No repetir el backup salvo que la base haya recibido escrituras desde este
corte.

### 2. Instalar servidor y migrador como una unidad

```bash
sudo install -o root -g root -m 0755 \
  target/release/jellyrin-server /usr/local/bin/jellyrin-server
sudo install -o root -g root -m 0755 \
  target/release/jellyrin-migrate /usr/local/bin/jellyrin-migrate
sudo sha256sum /usr/local/bin/jellyrin-server /usr/local/bin/jellyrin-migrate
```

Los hashes instalados deben coincidir con los release. No iniciar si solo uno
de los dos coincide.

### 3. Aplicar 120 y arrancar de forma ordenada

```bash
sudo systemctl start jellyrin.service
sudo systemctl show jellyrin-migrate.service jellyrin.service \
  -p Id -p ActiveState -p SubState -p ExecMainStatus -p Result -p NRestarts
sudo journalctl -u jellyrin-migrate.service -u jellyrin.service \
  --since "2026-08-10 22:35:00 UTC" --no-pager
```

`jellyrin.service` tiene `Requires/After=jellyrin-migrate.service`: el migrador
debe terminar con status 0 antes de que el servidor arranque. Ante fallo, no
forzar reinicios repetidos ni ejecutar SQL manual de la migración.

### 4. Verificar esquema y frontera de seguridad

Comprobar como administrador, sin mostrar URLs ni credenciales:

```sql
SELECT max(version) FROM _sqlx_migrations WHERE success;

SELECT table_name, privilege_type
FROM information_schema.role_table_grants
WHERE grantee = 'jellyrin_runtime'
  AND table_name IN (
    'media_item_query_filter_summary_values',
    'media_item_query_filter_summary_coverage',
    'media_item_query_filter_summary_revisions'
  )
ORDER BY table_name, privilege_type;

SELECT p.proname, p.prosecdef, p.proconfig
FROM pg_proc AS p
JOIN pg_namespace AS n ON n.oid = p.pronamespace
WHERE n.nspname = 'public'
  AND p.proname IN (
    'jellyrin_rebuild_query_filter_summary',
    'jellyrin_reconcile_query_filter_summary_item',
    'jellyrin_mark_query_filter_summary_dirty'
  )
ORDER BY p.proname;
```

Gate:

- versión máxima `202608080120`;
- runtime con `SELECT` solamente sobre las tres tablas del resumen;
- funciones de publicación `SECURITY DEFINER` y `search_path` equivalente a
  `pg_catalog, public, pg_temp`;
- `PUBLIC` sin ejecución y runtime solo con las funciones estrechas previstas;
- coverage y revisiones existentes siguen reconciliadas o fallan cerrado hacia
  la proyección exacta.

La prueba destructiva de spoofing ya pasó en una base aislada. Si se repite en
staging, hacerlo dentro de una transacción explícita y `ROLLBACK`, nunca sobre
filas sin una selección previa ni dejando la revisión dirty.

### 5. Smokes operacionales

```bash
curl -fsS http://127.0.0.1:8096/health
curl -fsS http://127.0.0.1:8096/readyz
curl -4 -fsS https://jellyrin.test.kode.live/health
curl -4 -fsS https://jellyrin.test.kode.live/readyz
sudo systemctl show jellyrin.service -p ActiveState -p SubState -p NRestarts
```

Después, con una sesión autenticada existente y sin imprimir tokens:

1. abrir Movies y Series y confirmar páginas/totales/filtros;
2. pedir `PlaybackInfo` de una película compatible y verificar DirectProxy;
3. comprobar `Range` (`206`) sin proceso FFmpeg;
4. reproducir un episodio incompatible por HLS y confirmar el límite de un job,
   dos threads y `CPUQuota=150%`;
5. comprobar Live TV con un canal conocido sano; mantener como incidencia de
   upstream el canal que ya fallaba;
6. confirmar que Xtream y MAGSTV siguen registrados y que ningún log contiene
   credenciales, URLs autenticadas ni tokens.

Registrar latencia, delivery mode, número de procesos FFmpeg, CPU/RSS y errores
del journal. No pegar credenciales en terminal, documentación o chat.

### 6. Rollback

- Antes de aplicar 120: restaurar ambos binarios `pre-c89ccd8` y arrancar basta,
  porque el esquema sigue en 119.
- Después de aplicar 120: **no** arrancar el binario anterior contra 120. Parar
  Jellyrin y restaurar como conjunto el snapshot PostgreSQL pre-120 y ambos
  binarios anteriores usando las credenciales root-only del procedimiento de
  recuperación. Verificar primero checksums/decryptabilidad; el script
  `ops/postgres/restore-drill.sh` sirve para validar en una base aislada, no para
  sobrescribir directamente la base productiva.
- Mantener el servicio detenido si la restauración no puede demostrarse
  completa. Nunca marcar manualmente 120 como aplicada ni editar
  `_sqlx_migrations`.

### 7. Cerrar documentación y Git

Tras los gates:

1. actualizar `docs/transcode-optimization-plan.md` de “120 validado localmente”
   a “120 desplegado”;
2. anotar duración de migración, hashes instalados, ruta/checksum del backup,
   ACL, health/readiness, `NRestarts` y smokes de reproducción;
3. ejecutar formato, `git diff --check` y revisar que no aparezcan secretos;
4. crear un commit separado de evidencia operacional;
5. decidir con el usuario cuándo empujar los 52 commits locales a GitHub.

## Gates de salida

El rollout solo se considera terminado si todos se cumplen:

- esquema 120 aplicado una sola vez por el migrador;
- servidor y migrador instalados corresponden al mismo commit/hash;
- runtime sin DML directo sobre el resumen;
- health/readiness local y HTTPS en 200;
- `NRestarts=0` después del arranque estable;
- catálogo Movies/Series y filtros correctos;
- DirectProxy/Range no inicia FFmpeg;
- HLS incompatible funciona dentro de los límites de CPU;
- logs/DB/argv sin secretos;
- plan principal actualizado y rollback conservado.

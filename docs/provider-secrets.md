# Credenciales de proveedores externos

Jellyrin guarda las credenciales Xtream y las credenciales core de tuners de plugins una sola vez
en `provider_secrets`. Las configuraciones de plugin, tuner y `livetv` conservan únicamente
`JellyrinProviderSecretRef`; las lecturas públicas no descifran esa referencia. Un campo opaco y
propio del proveedor, por ejemplo el `SecretReference` de MAGSTV, sigue siendo configuración
pública de routing y no se confunde con `JellyrinProviderSecretRef`.

`Username`, `UserName` y `Password` solo se admiten con un `Type` explícito `xtream` o
`plugin:<id>`. Tanto PostgreSQL como el adaptador SQLite de migración/tests fallan antes de escribir
si el tipo falta o es desconocido, incluso si el valor está vacío o enmascarado. Esto evita que una
ruta bulk o un cliente con casing distinto pueda dejar credenciales en JSON plano.

Cada fila usa AES-256-GCM con nonce aleatorio y AAD ligada a versión, key-id, proveedor e id del
secreto. La clave nunca se genera ni se guarda en la base de datos. Si hay secretos cifrados y no
se configura una clave, el servidor falla al arrancar. Si una instalación antigua aún contiene
`Username`/`Password`, el arranque valida que las tres copias Xtream coincidan y las sustituye en
una única transacción por la misma referencia central, sin llamar al proveedor remoto.

Las escrituras de configuración de plugin, tuner y `livetv`, la restauración de snapshots y el
backfill cifran el envelope y guardan su referencia usando la misma transacción de base de datos.
Si falla cualquier `INSERT`/`UPDATE`, ambos cambios hacen rollback. El helper interno de pruebas
que ejercita una protección aislada usa copy-on-write: al cambiar credenciales crea otra referencia
y nunca sobrescribe el secreto que sigue usando la configuración vigente. No forma parte de la API
del adapter; los writers transaccionales son la única ruta de producción soportada.

## Grants para plugins externos

Todo plugin `ExternalProcess` que publique la capacidad `LiveTvProvider` queda dentro de la
frontera sensible, aunque omita `ProviderSecrets` en su manifest. Por ello no recibe credenciales
de cuenta/tuner dentro de `TunerConfig`, argumentos, prefijos de variables de entorno ni
configuración genérica, y omitir el permiso no permite eludir ese rechazo. El manifest puede
enumerar variables operativas exactas revisables, pero no barrer prefijos. Para `LiveTvProvider`
también se rechazan nombres exactos con forma de credencial de cuenta —por ejemplo `USERNAME`,
`PASSWORD`, `SECRET_KEY`, `API_KEY` o `ACCESS_TOKEN`—; valores operativos concretos como una
identidad de dispositivo o un secreto de firma del protocolo siguen siendo configuración de un
paquete nativo controlado, no credenciales del tuner. Aun así, el core nunca
propaga `DATABASE_URL`, claves del vault, variables PostgreSQL, credenciales cloud/CI ni sus
prefijos protegidos. Los binarios siguen sin ser una sandbox de SO. `ProviderSecrets` solo
habilita la opción de recibir un grant: el plugin debe declararlo y un administrador debe
concederlo; ambas condiciones se validan
antes de abrir el vault. Los runtimes DotNet y los plugins legacy `ChannelProvider` no pueden usar
esta frontera.

El alta o actualización de un tuner sigue este orden:

1. valida `Type`, identidad del plugin, runtime, manifest, permiso concedido y referencia enviada;
2. hace overlay sobre la configuración server-side para que un cambio password-only no borre URL,
   categorías ni routing;
3. bajo el lock de escritura del plugin, cifra y persiste primero, vuelve a leer la configuración
   redactada y detiene cualquier host persistente que pudiera conservar estado anterior;
4. justo antes de `ImportChannels` o `ResolvePlayback`, toma el lock de lectura del plugin, relee la
   configuración canónica persistida, descifra en memoria y crea un `LiveTvProviderSecretGrant`
   ligado al id de plugin, id de tuner, acción, id de secreto y revisión;
5. ejecuta toda llamada que contenga `SecretGrant` en un proceso one-shot y lo destruye al
   terminar. Playback usa el lane `provider-secret`; una importación paginada usa
   `catalog-import` y conserva el mismo proceso solo durante todas sus páginas. La importación tiene
   un deadline global de 120 segundos, máximo 256 páginas, 100.000 canales, 10.000 categorías,
   tokens de continuación de 4 KiB, 1 MiB por mensaje RPC y 64 MiB agregados de JSON codificado.
   Esa última cifra acota payload, no RSS, porque el árbol JSON tiene overhead. El grant nunca se
   persiste ni pasa a un host reutilizable.

La referencia aportada por un cliente debe coincidir exactamente con el estado de ese mismo tuner;
no se puede copiar una referencia de otro tuner ni sustituirla desde la API. Playback convierte
cualquier fallo de esta frontera en un `503` genérico. Un lock R/W compartido por la identidad
normalizada del plugin mantiene una lectura desde la recarga canónica hasta que termina la llamada
y, para import, hasta confirmar snapshot y configuración pública;
cambiar permisos, rotar credenciales, actualizar `livetv`, habilitar/deshabilitar/desinstalar el
plugin o borrar el tuner toma el lado de escritura durante la mutación e invalidación del host. Así
una revocación o rotación no puede competir con una invocación que use estado anterior.

Los tipos sensibles del SDK y las credenciales descifradas limpian sus asignaciones al destruirse;
los buffers JSON del transporte RPC también se limpian y sus `Debug` están redactados. La
importación paginada mantiene una sola copia JSON del request y la limpia al salir. Esto reduce la
residencia de secretos, pero no constituye una garantía de borrado de todas las copias que pudiera
crear el allocator, el kernel o un plugin externo.

El detector fail-closed reconoce variantes de usuario, password, passphrase, secret, token, API
key, authorization, cookie, credential y private key, además de userinfo y parámetros sensibles en
URLs parseables. Se aplica a configuración genérica y a salida del proveedor. Los canales externos
se reconstruyen mediante un esquema seguro: no se aceptan `ImageUrl` ni `MediaStreams`, y
los campos de texto, `ProviderIds` y categorías quedan acotados, sin controles ni valores URL. El
core rechaza además
cualquier respuesta de una llamada con grant que refleje una credencial concedida como canario,
aunque aparezca bajo una clave aparentemente inocente.

El tracing HTTP registra método y path, nunca la URI completa ni su query. Esto es obligatorio
porque clientes Jellyfin todavía envían `api_key` en la query de streams, imágenes y otros
recursos; ese token no debe convertirse en un campo heredado por los logs de request/response.

## Keyring recomendado

Monte un fichero regular con modo `0400` o `0440` y esta forma:

```json
{
  "active_key_id": "2026-08",
  "keys": {
    "2026-08": "BASE64_DE_32_BYTES"
  }
}
```

Configure `JELLYRIN_PROVIDER_SECRET_KEYRING_FILE` con su ruta. También se admite una clave única
mediante `JELLYRIN_PROVIDER_SECRET_KEY` o `JELLYRIN_PROVIDER_SECRET_KEY_FILE`, junto con
`JELLYRIN_PROVIDER_SECRET_KEY_ID`, pero el keyring permite rotación online. No configure más de una
fuente a la vez.

El lector limita tanto el tamaño declarado como la lectura real a 128 KiB. En Unix rechaza enlaces
simbólicos, abre con `O_NOFOLLOW`, valida sobre el descriptor que sea un fichero regular con
permisos privados y comprueba que device/inode no cambiaron entre inspección y apertura. Así una
sustitución concurrente no convierte el path validado en otro fichero.

Para rotar, añada una clave nueva, márquela como `active_key_id` y mantenga la anterior en `keys`.
Al reiniciar, Jellyrin descifra con la clave antigua y vuelve a cifrar con la activa, incrementando
la revisión que invalida probes derivados. La rotación completa usa una sola transacción: si una
fila no puede rotarse, ninguna queda a medias. Tras un arranque correcto y comprobar el log de
rotación, retire la clave anterior en un segundo despliegue. Un fallo de autenticación AEAD es
genérico y no incluye credenciales.

El contenedor usa de forma estable UID/GID `10001:10001`. Para Compose, cree el fichero host como
`root:10001` y modo `0440` (por ejemplo, `sudo chown root:10001 <ruta> && sudo chmod 0440 <ruta>`);
así Jellyrin puede leerlo sin hacerlo accesible a otros usuarios. El overlay
[docker-compose.provider-secrets.yml](../docker-compose.provider-secrets.yml) lo monta sin
introducir su valor en Compose; actívelo junto al fichero principal con
`docker compose -f docker-compose.yml -f docker-compose.provider-secrets.yml up -d`. Si se cambia
deliberadamente la identidad de la imagen, debe ajustarse el grupo del fichero antes de arrancar.
Para systemd, el unit principal ya usa `LoadCredential`: cree
`/etc/jellyrin-secrets` como `root:root` modo `0700` y guarde
`provider-secret-keyring.json` como `root:root` modo `0400`. PID 1 entrega al
servicio una copia inmutable bajo `%d`; no añada la variable al fichero de
entorno ni guarde la fuente dentro de `ConfigurationDirectory`.

Al borrar un tuner, PostgreSQL y SQLite eliminan su envelope en la misma transacción únicamente si
la referencia exacta `secret_id + provider` ya no aparece en ningún tuner, configuración de plugin
o configuración nombrada. PostgreSQL bloquea tuner y envelope; SQLite usa `BEGIN IMMEDIATE`; una
referencia compartida se conserva y nunca se decide por `LIKE`.

Además, cada arranque ejecuta una reconciliación global de envelopes históricos o creados por otros
caminos. Recorre de forma anidada todas las configuraciones de tuners, plugins y configuraciones
nombradas, y compara la identidad exacta `secret_id + provider` ignorando únicamente `Revision`.
PostgreSQL usa una transacción serializable y bloquea los envelopes candidatos; SQLite serializa
el barrido con `BEGIN IMMEDIATE`. Si cualquier JSON o referencia no se puede interpretar, la
transacción aborta antes de borrar y el servidor conserva todos los candidatos, registra el aviso y
continúa arrancando. Así una configuración reparable puede retener de más, pero no provocar la
pérdida de credenciales cifradas.

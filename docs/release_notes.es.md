*Esta es una traducción de las notas de las versiones 0.2.1, 0.2.0 y 0.1.8. Las versiones anteriores
están en [release_notes.md](release_notes.md) (inglés), que prevalece en caso de diferencia.*

# romexplorer 0.2.2 (alpha)

Alfa: el esquema de la base de datos puede cambiar entre versiones sin forma de actualizarlo.
Haz una copia de seguridad antes de sustituir una versión.

## Cambios

**Audio de las ROM**
- El índice de música de las ROM puede leer varias ROM a la vez (4 por defecto, se cambia en los ajustes de audio).
- El índice de música de las ROM lee solo la copia mejor clasificada de cada título, y las demás solo si esa no tiene música.
- En los juegos de varios discos, ya no se vuelven a leer los archivos de música que tiene un disco anterior.
- Los juegos de cartucho cuya música la aplicación ya conoce la reproducen en cuanto se abre la página del título, sin esperar al índice.

# romexplorer 0.2.1 (alpha)

Alfa: el esquema de la base de datos puede cambiar entre versiones sin forma de actualizarlo.
Haz una copia de seguridad antes de sustituir una versión.

## Cambios

**3DS**
- Las ROM de 3DS se pueden convertir a otros formatos.
- No se incluyen claves. Pon aes_keys.txt (y seeddb.bin si un título lo necesita) junto a los
  archivos, junto a romexplorer o en su carpeta assets.
- Coincidencia por número de serie para las ROM de 3DS.

**Audio de las ROM**
- Al abrir la página de un título, ahora se indexa primero la música de las ROM de ese juego.
- El índice de música de las ROM ahora lee primero las ROM más pequeñas y omite las no compatibles.
- Se admiten los juegos de PS1 con motor de sonido propio.

## Correcciones

- El índice de música de las ROM parecía detenido mientras una ROM tardaba en leerse.
- El índice de música de las ROM omitía las ROM identificadas solo por una coincidencia con un DAT.
- Un escaneo dejaba sin plataforma las ROM japonesas de Super Famicom cuyo título interno está en katakana.
- La página de un título repetía la misma música por cada copia del juego que encontraba.
- La indexación de la música de las ROM no terminaba nunca en algunas imágenes de disco.
- Al convertir volcados de SNES en una copia, se convertía igualmente un archivo cuyo tamaño y
  cabecera interna no coincidían sobre la cabecera de copiadora.

**Escaneo**
- En Linux, un escaneo pasaba por alto entradas de cue, gdi y m3u con otras mayúsculas o con «\»
  como separador, y tomaba dos archivos que solo difieren en mayúsculas por una sola ROM.
- Al escanear una carpeta se ignoraban el tipo y la plataforma de sus subcarpetas.
- Los reescaneos volvían a leer los juegos en carpeta, separaban pistas y perdían coincidencias por
  número de serie. Agrupar discos en carpetas grandes es más rápido.
- Los archivos que no son ROM (texto, imágenes) ahora solo se convierten en juegos al identificarse.
- Ahora uno de crc32, md5 y sha1 debe seguir activo en los hashes del escaneo. Con solo xxh3_128 se
  perdían las coincidencias con el DAT.

**Copia**
- Al sobrescribir se borraba el archivo antiguo antes de escribir el nuevo, una copia reescribía
  listas de reproducción que debían conservarse y dos archivos que solo difieren en mayúsculas se
  escribían en uno.

**Unión de títulos**
- La unión juntaba versiones de un conjunto de versiones, o títulos comerciales con no
  licenciados. «Aceptar todo» devolvía uniones deshechas y volvían enlaces de IGDB quitados por la
  auditoría.
- Una coincidencia de Hasheous por el otro nombre de un juego unido creaba un título duplicado.

**Servidor y tareas**
- Obtener arte para un cliente podía llegar a la red interna del servidor. También se añadió un
  límite de tamaño.
- Descargar las cachés SQLite «database» y «all» ahora requiere un administrador.
- Una sincronización completa interrumpida dejaba una tabla a medias, y tras cancelar una copia de
  seguridad la siguiente empezaba mientras aún se escribía.
- Las copias a otra unidad y las sincronizaciones desde otro servidor esperaban a tareas ajenas, y
  tras reiniciar la base de datos una tarea podía empezar dos veces.

**Otros**
- La música de las ROM fallaba con un set PSF2.
- Partes de la consola web y de la pantalla de ajustes seguían en inglés.
- MariaDB anterior a 10.6 ahora muestra un error al conectar.

# romexplorer 0.2.0 (alpha)

## Cambios

**IA en Revisión de fusiones**
- «Preguntar a Google» es ahora un menú Ask. Abre el modo IA de Google, Claude, ChatGPT o
  Perplexity en el navegador con la pregunta ya escrita.
- Las respuestas en la app pueden venir de Gemini, Claude, OpenAI o Perplexity, cada uno con su
  clave de API y modelo.
- Las respuestas ahora se guardan. «Volver a preguntar» obtiene una nueva; las fijadas nunca se
  vuelven a pedir.
- Las respuestas guardadas se incluyen en las copias de seguridad de la base de datos.

**Config › AI**
- Se añadió una pestaña de ajustes de IA.
- Muestra el número de respuestas guardadas y el coste estimado de este mes y en total. Las
  respuestas se pueden exportar e importar en JSON.

**IA en la pestaña Definitiva**
- La página de cada juego puede preguntar a una IA en qué se diferencian sus versiones, para elegir
  la definitiva. Las respuestas reciben los mismos rangos que las notas.
- Antes de la primera pregunta se muestra un aviso de coste. Config › AI enlaza la página de límites
  de uso de cada proveedor.
- «Valorar en lote con IA» pregunta por varios juegos en una sola tarea. Antes muestra el número de
  preguntas y el coste estimado, y omite los juegos ya respondidos.
- Los juegos con una versión nueva tras su valoración se marcan como «Versión nueva desde la
  valoración» y se pueden excluir con el filtro.

**Títulos: versiones menores y grupos de versiones**
- Se añadió «Ocultar versiones menores».
- Se añadió «Una de cada conjunto de versiones».
- Se añadió una tarjeta «Versiones» a las páginas de título.

**Config › Avanzado**
- Se añadió una pestaña que reúne las acciones que pueden dañar la base de datos o perder archivos.
  Optimizar la base de datos se trasladó aquí desde Tareas y datos › DB.

**Audio de las ROM**
- Las páginas de título pueden reproducir la música de las ROM (pistas de audio CD de juegos en
  disco y archivos de sonido de Nintendo DS y 3DS).
- Los criterios de qué pistas se reproducen se pueden ajustar en los ajustes.
- Se añadió un botón «no reproducir esta pista» al reproductor de la página de título.
- La música de las ROM decodificada para reproducir se guarda en caché en memoria (Ajustes › Audio,
  256 MiB por defecto).
- Se ajustó la normalización del reproductor.
- El reproductor puede hacer fundido cruzado (2000 ms por defecto, 0 lo desactiva).
- Ahora se recorta el silencio al final de las pistas.
- La música de las páginas de título ahora está activada por defecto. En el primer inicio, tras el
  idioma, la app pregunta si reproducir música. Los títulos sin audio propio reproducen una pista al
  azar por defecto.

**Revisión de fusiones y Títulos combinados**
- Un lanzamiento de una tarjeta se puede arrastrar al título correcto. Soltarlo en «+» crea un
  título nuevo.
- Las tarjetas aceptadas y rechazadas se pueden mostrar con el filtro de estado. Por defecto están
  ocultas.
- Las separaciones también se registran en Editar › Títulos combinados y se pueden deshacer allí.
  También aparecen las hechas en Revisión de fusiones antes de esta actualización.

**Lanzamientos combinados**
- Los discos de un juego registrados como lanzamientos separados se pueden seleccionar y unir con
  «Combinar en un solo lanzamiento».

**Escaneo**
- Se añadió «Volver a escanear este archivo» a las páginas de archivo. Vuelve a leer el archivo junto
  con los demás archivos de la ROM y asocia el juego a su lanzamiento al momento.
- Las páginas de archivo ahora muestran el lanzamiento y el título enlazados.

## Correcciones

- Los nombres con un guion dentro de una palabra no coincidían con la grafía de IGDB, así que
  algunos juegos no encontraban coincidencia en IGDB.
- Las sincronizaciones de IGDB de SNES y NES omitían juegos lanzados solo en Japón.
- Desenlazar IGDB dejaba en el título los nombres alternativos de IGDB, sin forma de borrarlos.
  Ahora se van con el enlace y cada uno se puede borrar con ✕.
- Los enlaces de IGDB quitados a mano se volvían a enlazar con «Actualizar metadatos». Ahora siguen
  desenlazados hasta enlazarlos de nuevo a mano.
- Los subconjuntos de RetroAchievements (desafíos, speedruns, etc.) entraban en el título del juego
  y podían juntar versiones distintas en un título.
- Deshacer una fusión hecha en Revisión de fusiones no devolvía la sugerencia.
- Las sugerencias de títulos ya fusionados a mano seguían en Revisión de fusiones.
- Los juegos piratas y no licenciados se fusionaban con títulos comerciales.
- Las tarjetas «otro juego dentro» dejaban en el título juegos piratas y no licenciados de otras
  regiones.
- Las tarjetas «otro juego dentro» tomaban por juegos distintos los lanzamientos con un número
  delante del nombre («0743 - ...»).
- Tareas como la auditoría de fusiones ahora muestran un progreso detallado.
- En Windows arm64 y Linux arm64 no se podían convertir imágenes de Wii U, Xbox y Xbox 360, leer
  archivos PBP ni reproducir la música de las ROM.
- Los archivos que figuran en un DAT quedaban sin enlazar a ningún lanzamiento ni título.

# romexplorer 0.1.8 (alpha)

## Cambios

**Medios**
- Empezar de nuevo con una base de datos nueva ahora reutiliza el arte que ya está en la carpeta de
  medios.

**Tareas**
- Se ajustaron los detalles de la lista de tareas.
- Se añadió un botón «Ejecutar a continuación» a las tareas en espera.

**Coincidencia de hash**
- La coincidencia de hash solo con Hasheous ahora también obtiene datos y arte de IGDB.
- serial_match ahora puede elegir por plataforma si usar los números de serie de los nombres de
  archivo.

**Revisión de fusiones**
- Las respuestas de Gemini y «Preguntar a Google» ahora usan el idioma de la interfaz.
- Una consulta fallida ahora muestra el motivo.
- Ahora se pregunta a Gemini aunque no se pueda leer su lista de modelos.

**Definitiva y Crossplat**
- Ahora solo se muestran los títulos con ID de IGDB.

## Correcciones

**Colección**
- La búsqueda en la pestaña Definitiva solo encontraba el nombre del título principal.

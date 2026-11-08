*Esta es una traducción de las notas de las versiones 0.2.0 y 0.1.8. Las versiones anteriores
están en [release_notes.md](release_notes.md) (inglés), que prevalece en caso de diferencia.*

# romexplorer 0.2.0 (alpha)

Alfa: el esquema de la base de datos puede cambiar entre versiones sin forma de actualizarlo.
Haz una copia de seguridad antes de sustituir una versión.

## Cambios

**IA en Revisión de fusiones**
- «Preguntar a Google» es ahora un menú Ask: el modo IA de Google, Claude, ChatGPT y Perplexity se
  abren en el navegador con la pregunta ya escrita.
- La respuesta dentro de la aplicación puede venir de Gemini, Claude, OpenAI o Perplexity, cada uno
  con su clave de API y su modelo.
- Las respuestas se guardan. «Volver a preguntar» obtiene una nueva; las respuestas fijadas no se
  vuelven a pedir.
- Las respuestas guardadas forman parte de la copia de seguridad de la base de datos.

**Config › AI**
- Una pestaña nueva para los ajustes de la IA.
- Muestra cuántas respuestas hay guardadas y su coste aproximado este mes y en total. Las
  respuestas se pueden exportar e importar en JSON.

**IA en la pestaña Definitiva**
- La página de cada juego puede preguntar a la IA qué versión tiene más contenido y mejores
  gráficos, sonido y comodidad de juego. La respuesta trae un rango, como las notas revisadas.
- Una respuesta obtenida con un enlace del navegador se puede pegar, o guardar desde el
  portapapeles con un clic.
- La primera pregunta muestra un aviso de coste. Config › AI enlaza a la página de límites de gasto
  de cada proveedor.
- «Valorar en lote con IA» pregunta por varios juegos en una sola tarea: los juegos elegidos en la
  pestaña Definitiva, o las filas elegidas de un set o el set entero. Primero muestra el número de
  preguntas y un coste aproximado, salta los juegos ya respondidos y se puede programar para un set.
- Un rango dado antes de que saliera una versión más nueva del juego se marca «Nueva versión desde
  la valoración», y el filtro puede descartar esos rangos.

**Títulos: versiones menores y grupos de versiones**
- «Ocultar versiones menores» oculta un juego cuando hay una edición más completa en la misma plataforma
  y región (International, Director's Cut, Final Mix, etc.; una Easy Type cede ante el juego
  normal). En Títulos, Lanzamientos y la lista de lanzamientos de un título.
- «Una de cada conjunto de versiones» muestra una sola versión de los juegos que salieron en varias
  versiones (Pokémon, Yo-kai Watch, etc.): la que tienes, si no la que elegiste, si no una al azar,
  con 🎲 para ver otra. Las versiones de un mismo grupo ya no forman una sola familia en la pestaña
  Definitiva.
- La nueva tarjeta Versiones de la página de un título muestra ambos y permite corregirlos.

**Config › Avanzado**
- Una pestaña nueva para las operaciones que pueden dañar la base de datos o hacer perder
  archivos. Optimizar DB se ha movido aquí desde Tareas y datos › DB.

**Audio de las ROM**
- Las páginas de título pueden reproducir la música que hay dentro de tus ROM (solo plataformas y
  tipos de soporte compatibles).
- En los ajustes se elige qué pistas se reproducen.
- Se admiten los formatos de audio originales (NSF, SPC, GBS, VGM, HES, KSS, AY, SAP y SGC
  de Game Gear, sueltos o en zip).
- Una biblioteca de audio original: indica en Ajustes › Audio las carpetas donde guardas los sets, e «Index
  ROM audio» empareja cada set con tus ROM por su contenido. Un mismo set suena en todas las
  regiones del juego, se llame como se llame.
- El reproductor de las páginas de título tiene un botón «no reproducir esta pista». Las pistas
  ocultas aparecen bajo la lista de reproducción: un clic reproduce una y Restaurar la recupera.
- La música de las ROM decodificada para reproducir se guarda en caché en memoria (Ajustes › Audio,
  256 MiB por defecto). Cuando se llena, las pistas pasan a la caché de audio en disco, que usa la
  carpeta de la caché de nivel 1 si está configurada.
- Se ha ajustado la normalización del reproductor.
- El reproductor puede hacer fundido cruzado (2000 ms por defecto, 0 para desactivarlo).
- Se corta el silencio al final de una pista: las pistas de las ROM se recortan, y en los archivos
  de audio el escaneo de audio mide dónde termina el sonido (vuelve a ejecutarlo para los archivos
  escaneados antes).
- La música de las páginas de título está activada por defecto, y el primer inicio pregunta si la
  quieres, justo después del idioma. Un título sin audio propio reproduce pistas al azar por
  defecto.
- Los archivos PSF y PSF2 usan una BIOS de PS2 de tu colección, y se saltan si no hay ninguna.

**Revisión de fusiones y Títulos combinados**
- Los lanzamientos de una tarjeta se pueden arrastrar al título al que pertenecen, o a «+» para un
  título nuevo con el nombre del primero. También funciona tocar un lanzamiento y luego un título.
- Las tarjetas ya aceptadas o rechazadas se pueden volver a mostrar con el nuevo filtro de estado.
  Están ocultas por defecto.
- Las separaciones aparecen ahora en Editar › Títulos combinados y se pueden deshacer allí. Las
  hechas desde Revisión de fusiones antes de esta actualización se añaden a la lista.

**Lanzamientos combinados**
- Los discos de un mismo juego listados por separado (los de un pack, un disco extra) se pueden unir con «Combinar en un solo lanzamiento». Se lista un lanzamiento para todos y al copiarlo se copian todos los discos. Desde la página del lanzamiento se puede separar uno.

## Correcciones

- Algunos juegos no encontraban coincidencia en IGDB: un nombre con un guion dentro de una palabra no coincidía con la grafía de IGDB, y la sincronización de IGDB para SNES y NES omitía los juegos lanzados solo en Japón.
- Un título conservaba los títulos alternativos de IGDB tras quitar su vínculo con IGDB, y no se podían borrar. Ahora se quitan con el vínculo, y cualquier título alternativo se puede quitar con ✕.
- «Actualizar metadatos» volvía a vincular con IGDB un título cuyo vínculo se había quitado a mano. Ahora sigue sin vincular hasta que lo vincules tú.
- Los subsets de RetroAchievements (desafío, speedrun y similares) entraban en el título de su juego y podían unir dos versiones de un juego en un título. Cada subset es ahora un título propio; la auditoría de fusiones separa los que ya estaban.
- Al deshacer una fusión aceptada en la revisión de fusiones, la sugerencia no volvía. Ahora vuelve a quedar pendiente.
- Una sugerencia de fusión seguía en la revisión de fusiones después de fusionar sus títulos a mano.
- Juegos piratas y sin licencia se fusionaban en los títulos de juegos comerciales.
- La tarjeta «otro juego dentro» dejaba en el título juegos piratas y sin licencia de otras
  regiones.
- La tarjeta «otro juego dentro» tomaba por otro juego un lanzamiento con un número delante del
  nombre («0743 - ...»).
- Tareas como la auditoría de fusiones mostraban poco de su progreso. Ahora lo muestran en detalle.

# romexplorer 0.1.8 (alpha)

Alfa: el esquema de la base de datos puede cambiar entre versiones sin forma de actualizarlo.
Haz una copia de seguridad antes de sustituir una versión.

## Cambios

**Medios**
- Al empezar de nuevo con una base de datos nueva, se usan las imágenes que ya están en la carpeta
  de medios en lugar de descargarlas otra vez.

**Tareas**
- Se han ajustado las columnas de detalle de la lista de tareas.
- Una tarea en espera tiene un botón «Ejecutar a continuación», también para una selección: empieza en cuanto
  termina la tarea en curso de su cola. Las demás colas no se ven afectadas.

**Coincidencia de hash**
- Una coincidencia de hash con solo Hasheous activado también obtiene los datos de IGDB y las
  imágenes de lo que encontró, como con PlayMatch.
- El ajuste serial_match decide por plataforma si cuenta el número de serie del nombre de archivo.
  Una plataforma que se deja como está sigue la casilla del nombre de archivo.

**Revisión de fusiones**
- La respuesta de Gemini y «Preguntar a Google» llegan en el idioma de la interfaz.
- Cuando la consulta falla, el mensaje dice por qué: un certificado o un tiempo de espera, el
  modelo y la cuota agotada. Una clave sin cuota gratuita para el modelo lo indica.
- Si no se puede leer la lista de modelos, se pregunta a Gemini de todos modos; ante un error de
  cuota, se prueba una vez el siguiente modelo Flash.

**Definitiva y Crossplat**
- Solo se listan los títulos con un ID de IGDB. Los títulos de Wii U nombrados por su ID de título
  ya no aparecen en Definitiva.

## Correcciones

**Colección**
- La búsqueda de la pestaña Definitiva solo encontraba el nombre del título principal. Ahora
  encuentra una familia por el nombre de cualquier versión, incluidos los nombres localizados y
  alternativos.

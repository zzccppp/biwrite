; BiWrite in Explorer's "Open with" for .tex, .md, .markdown and .txt files.
; Only BiWrite's own keys are written: no file type's default program changes
; (Tauri's own file associations would make BiWrite the default).

; The hooks are included before Tauri defines MAINBINARYNAME: it is used
; only inside the macros, which expand where they are inserted.
!define BIWRITE_PROGID "BiWrite.Document"

!macro BIWRITE_OPEN_WITH EXT
  WriteRegStr SHCTX "Software\Classes\.${EXT}\OpenWithProgids" "${BIWRITE_PROGID}" ""
  WriteRegStr SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe\SupportedTypes" ".${EXT}" ""
!macroend

!macro BIWRITE_NOT_OPEN_WITH EXT
  DeleteRegValue SHCTX "Software\Classes\.${EXT}\OpenWithProgids" "${BIWRITE_PROGID}"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr SHCTX "Software\Classes\${BIWRITE_PROGID}" "" "BiWrite document"
  WriteRegStr SHCTX "Software\Classes\${BIWRITE_PROGID}\DefaultIcon" "" "$INSTDIR\${MAINBINARYNAME}.exe,0"
  WriteRegStr SHCTX "Software\Classes\${BIWRITE_PROGID}\shell\open\command" "" '"$INSTDIR\${MAINBINARYNAME}.exe" "%1"'
  WriteRegStr SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe" "FriendlyAppName" "BiWrite"
  WriteRegStr SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe\shell\open\command" "" '"$INSTDIR\${MAINBINARYNAME}.exe" "%1"'
  !insertmacro BIWRITE_OPEN_WITH "tex"
  !insertmacro BIWRITE_OPEN_WITH "md"
  !insertmacro BIWRITE_OPEN_WITH "markdown"
  !insertmacro BIWRITE_OPEN_WITH "txt"
  ; Explorer reads the associations again.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro BIWRITE_NOT_OPEN_WITH "tex"
  !insertmacro BIWRITE_NOT_OPEN_WITH "md"
  !insertmacro BIWRITE_NOT_OPEN_WITH "markdown"
  !insertmacro BIWRITE_NOT_OPEN_WITH "txt"
  DeleteRegKey SHCTX "Software\Classes\${BIWRITE_PROGID}"
  DeleteRegKey SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe"
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend

# Legacy shim: icon builds live in scripts/make-icon.ps1
# (multi-size voice-mail.ico + voice-mail.png from assets/voice_mail.jpg).
& (Join-Path $PSScriptRoot 'scripts' 'make-icon.ps1')

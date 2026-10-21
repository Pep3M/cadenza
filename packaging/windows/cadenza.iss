; Cadenza installer for Windows (Inno Setup 6.3+), built by .github/workflows/release.yml:
;
;   iscc /DAppVersion=0.3.0 /DNumericVersion=0.3.0 /DDist=<folder with the binaries>
;        /DOutputDir=<folder> packaging\windows\cadenza.iss
;
; Installs for the current user by default (no administrator rights, in
; %LOCALAPPDATA%\Programs\Cadenza, the same folder install.ps1 uses) or for every user
; (Program Files). Start menu shortcut, optional desktop shortcut, optional `cadenza` on
; the PATH, .cdz / .cdzpack opened by cadenza-app, an uninstaller in Settings > Apps. The
; fixed AppId makes a newer installer upgrade the existing install in place. Projects,
; settings and the style library live in the user folders and are never removed.

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.0.0"
#endif
#ifndef Dist
  #define Dist SourcePath + "..\..\target\release"
#endif
#ifndef OutputDir
  #define OutputDir SourcePath + "..\..\dist"
#endif

[Setup]
AppId={{38A1D591-6CF8-4B24-9C3B-A1692FF02B06}
AppName=Cadenza
AppVersion={#AppVersion}
AppVerName=Cadenza {#AppVersion}
AppPublisher=Cadenza contributors
AppPublisherURL=https://github.com/Pep3M/cadenza
AppSupportURL=https://github.com/Pep3M/cadenza/issues
AppUpdatesURL=https://github.com/Pep3M/cadenza/releases
VersionInfoVersion={#NumericVersion}
VersionInfoProductName=Cadenza
VersionInfoDescription=Cadenza Setup
DefaultDirName={autopf}\Cadenza
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=..\..\LICENSE
SetupIconFile=..\icons\cadenza.ico
UninstallDisplayIcon={app}\cadenza-app.exe
UninstallDisplayName=Cadenza
WizardStyle=modern
ShowLanguageDialog=auto
ChangesAssociations=yes
ChangesEnvironment=yes
CloseApplications=yes
Compression=lzma2/max
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename=cadenza-{#AppVersion}-windows-x86_64-setup

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
#if FileExists(CompilerPath + "Languages\Spanish.isl")
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"
#endif
#if FileExists(CompilerPath + "Languages\BrazilianPortuguese.isl")
Name: "ptbr"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
#endif
#if FileExists(CompilerPath + "Languages\French.isl")
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"
#endif
#if FileExists(CompilerPath + "Languages\German.isl")
Name: "de"; MessagesFile: "compiler:Languages\German.isl"
#endif
#if FileExists(CompilerPath + "Languages\Italian.isl")
Name: "it"; MessagesFile: "compiler:Languages\Italian.isl"
#endif
#if FileExists(CompilerPath + "Languages\Russian.isl")
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"
#endif
#if FileExists(CompilerPath + "Languages\Japanese.isl")
Name: "ja"; MessagesFile: "compiler:Languages\Japanese.isl"
#endif
#if FileExists(CompilerPath + "Languages\Korean.isl")
Name: "ko"; MessagesFile: "compiler:Languages\Korean.isl"
#endif
; Simplified Chinese is an unofficial Inno Setup translation: the release workflow
; downloads it next to this script.
#if FileExists(SourcePath + "ChineseSimplified.isl")
Name: "zh"; MessagesFile: "ChineseSimplified.isl"
#endif

[CustomMessages]
en.Integration=Integration:
en.AddToPath=Add the cadenza command to the PATH (use it from a terminal)
en.Associate=Open .cdz projects and .cdzpack archives with Cadenza
#if FileExists(CompilerPath + "Languages\Spanish.isl")
es.Integration=Integración:
es.AddToPath=Añadir el comando cadenza al PATH (para usarlo desde una terminal)
es.Associate=Abrir los proyectos .cdz y los archivos .cdzpack con Cadenza
#endif
#if FileExists(CompilerPath + "Languages\BrazilianPortuguese.isl")
ptbr.Integration=Integração:
ptbr.AddToPath=Adicionar o comando cadenza ao PATH (para usá-lo em um terminal)
ptbr.Associate=Abrir projetos .cdz e arquivos .cdzpack com o Cadenza
#endif
#if FileExists(CompilerPath + "Languages\French.isl")
fr.Integration=Intégration :
fr.AddToPath=Ajouter la commande cadenza au PATH (pour l'utiliser dans un terminal)
fr.Associate=Ouvrir les projets .cdz et les archives .cdzpack avec Cadenza
#endif
#if FileExists(CompilerPath + "Languages\German.isl")
de.Integration=Integration:
de.AddToPath=Befehl cadenza zum PATH hinzufügen (zur Nutzung im Terminal)
de.Associate=.cdz-Projekte und .cdzpack-Archive mit Cadenza öffnen
#endif
#if FileExists(CompilerPath + "Languages\Italian.isl")
it.Integration=Integrazione:
it.AddToPath=Aggiungi il comando cadenza al PATH (per usarlo da un terminale)
it.Associate=Apri i progetti .cdz e gli archivi .cdzpack con Cadenza
#endif
#if FileExists(CompilerPath + "Languages\Russian.isl")
ru.Integration=Интеграция:
ru.AddToPath=Добавить команду cadenza в PATH (для работы в терминале)
ru.Associate=Открывать проекты .cdz и архивы .cdzpack в Cadenza
#endif
#if FileExists(CompilerPath + "Languages\Japanese.isl")
ja.Integration=統合:
ja.AddToPath=cadenza コマンドを PATH に追加する (ターミナルから使用)
ja.Associate=.cdz プロジェクトと .cdzpack アーカイブを Cadenza で開く
#endif
#if FileExists(CompilerPath + "Languages\Korean.isl")
ko.Integration=통합:
ko.AddToPath=cadenza 명령을 PATH에 추가 (터미널에서 사용)
ko.Associate=.cdz 프로젝트와 .cdzpack 아카이브를 Cadenza로 열기
#endif
#if FileExists(SourcePath + "ChineseSimplified.isl")
zh.Integration=集成:
zh.AddToPath=将 cadenza 命令添加到 PATH（在终端中使用）
zh.Associate=使用 Cadenza 打开 .cdz 项目和 .cdzpack 归档
#endif

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "associate"; Description: "{cm:Associate}"; GroupDescription: "{cm:Integration}"
Name: "addtopath"; Description: "{cm:AddToPath}"; GroupDescription: "{cm:Integration}"

[Files]
Source: "{#Dist}\cadenza-app.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Dist}\cadenza.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Dist}\cadenza-plugin-scanner.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\..\README.md"; DestDir: "{app}"

[Icons]
Name: "{autoprograms}\Cadenza"; Filename: "{app}\cadenza-app.exe"
Name: "{autodesktop}\Cadenza"; Filename: "{app}\cadenza-app.exe"; Tasks: desktopicon

[Registry]
; HKA = HKLM for an all-users install, HKCU for a per-user one.
Root: HKA; Subkey: "Software\Classes\.cdz"; ValueType: string; ValueName: ""; ValueData: "Cadenza.Project"; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.cdz\OpenWithProgids"; ValueType: string; ValueName: "Cadenza.Project"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Cadenza.Project"; ValueType: string; ValueName: ""; ValueData: "Cadenza project"; Flags: uninsdeletekey; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Cadenza.Project\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\cadenza-app.exe,0"; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Cadenza.Project\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\cadenza-app.exe"" ""%1"""; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.cdzpack"; ValueType: string; ValueName: ""; ValueData: "Cadenza.Archive"; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.cdzpack\OpenWithProgids"; ValueType: string; ValueName: "Cadenza.Archive"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Cadenza.Archive"; ValueType: string; ValueName: ""; ValueData: "Cadenza project archive"; Flags: uninsdeletekey; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Cadenza.Archive\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\cadenza-app.exe,0"; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Cadenza.Archive\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\cadenza-app.exe"" ""%1"""; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Applications\cadenza-app.exe\SupportedTypes"; ValueType: string; ValueName: ".cdz"; ValueData: ""; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Applications\cadenza-app.exe\SupportedTypes"; ValueType: string; ValueName: ".cdzpack"; ValueData: ""

[Run]
Filename: "{app}\cadenza-app.exe"; Description: "{cm:LaunchProgram,Cadenza}"; Flags: nowait postinstall skipifsilent

[Code]
const
  UserEnvKey = 'Environment';
  SystemEnvKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';

function EnvRoot: Integer;
begin
  if IsAdminInstallMode then
    Result := HKEY_LOCAL_MACHINE
  else
    Result := HKEY_CURRENT_USER;
end;

function EnvKey: String;
begin
  if IsAdminInstallMode then
    Result := SystemEnvKey
  else
    Result := UserEnvKey;
end;

{ Path without any entry naming Dir (case-insensitive, trailing backslash ignored). }
function PathWithout(Path, Dir: String): String;
var
  Rest, Part: String;
  P: Integer;
begin
  Result := '';
  Rest := Path;
  while Rest <> '' do
  begin
    P := Pos(';', Rest);
    if P = 0 then
    begin
      Part := Rest;
      Rest := '';
    end
    else
    begin
      Part := Copy(Rest, 1, P - 1);
      Rest := Copy(Rest, P + 1, Length(Rest));
    end;
    if (Part <> '') and
       (CompareText(RemoveBackslashUnlessRoot(Part), RemoveBackslashUnlessRoot(Dir)) <> 0) then
    begin
      if Result <> '' then
        Result := Result + ';';
      Result := Result + Part;
    end;
  end;
end;

{ Adds the install folder to the PATH (once) or removes it. }
procedure SetPathEntry(Add: Boolean);
var
  Path, Dir, Updated: String;
begin
  Dir := ExpandConstant('{app}');
  if not RegQueryStringValue(EnvRoot, EnvKey, 'Path', Path) then
    Path := '';
  Updated := PathWithout(Path, Dir);
  if Add then
  begin
    if Updated <> '' then
      Updated := Updated + ';';
    Updated := Updated + Dir;
  end;
  if Updated <> Path then
    RegWriteExpandStringValue(EnvRoot, EnvKey, 'Path', Updated);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    SetPathEntry(WizardIsTaskSelected('addtopath'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    SetPathEntry(False);
end;

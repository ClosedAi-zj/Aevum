; 恒晷 · Aevum Studio —— Inno Setup 6 安装脚本
;
; 定位：**绿色包仍然是主路径**，安装器只是多一条路。zip 解压即用、不写注册表、不需要管理员；
; 安装器解决的是另一批人的问题——开始菜单里找得到、`/SILENT` 能批量部署、卸载时知道
; 该删什么。两条路装出来的程序**完全一样**（同一个 aevum.exe + 同一个 tools/ffmpeg.exe）。
; 因此这里不做任何「只有安装版才有的功能」，也不写额外的注册表项。
;
; 编译（需先跑过 scripts/package.sh，产物在 dist/aevum）：
;   iscc /DMyAppVersion=1.9.6 scripts/install/aevum.iss
;   iscc scripts/install/aevum.iss            ; 版本号退回下面的默认值
; 产物：dist/installer/Aevum-Studio-<版本>-setup.exe
;
; 静默安装：`Aevum-Studio-1.9.6-setup.exe /SILENT /NORESTART`（Inno 原生支持；
; /VERYSILENT 则不显示进度窗）。静默模式下所有交互都自动走默认值，见 [Code] 里的
; WizardSilent() 分支。
;
; **不关联 .aevum 后缀**：本仓的工程文件不是一个带扩展名的文件，而是每个序列目录里的
; `<序列根>/.aevum/project.json`（见 crates/aevum-core/src/project.rs）。`.aevum` 是
; **目录名**，Windows 没有「双击目录用某个程序打开」的注册表机制，硬做只能在资源管理器
; 里挂一层外壳扩展——那是另一个组件、另一套权限、另一个崩溃面，为「双击打开」不值得。
; 真要加关联，得先有 `.aevumproj` 这样的单文件工程格式，那是产品决定，不是安装器能定的。
;
; Aevum Studio installer for Inno Setup 6.
;
; Positioning: **the portable zip stays the primary path**; the installer is simply another
; road. The zip is unzip-and-run, writes no registry keys and needs no administrator. The
; installer solves a different set of problems -- being findable in the Start menu, `/SILENT`
; for bulk deployment, and knowing what to remove on uninstall. Both roads produce **exactly
; the same program** (the same aevum.exe and the same tools/ffmpeg.exe), so nothing here is
; installer-only, and no extra registry keys are written.
;
; Build (run scripts/package.sh first; its output is dist/aevum):
;   iscc /DMyAppVersion=1.9.6 scripts/install/aevum.iss
; Output: dist/installer/Aevum-Studio-<version>-setup.exe
;
; Silent install: `Aevum-Studio-1.9.6-setup.exe /SILENT /NORESTART` (native to Inno;
; /VERYSILENT hides the progress window too). In silent mode every prompt takes its default;
; see the WizardSilent() branches in [Code].

#define MyAppName "恒晷 · Aevum Studio"
#define MyAppNameEn "Aevum Studio"
#define MyAppExeName "aevum.exe"
#define MyAppPublisher "Aevum Studio contributors"

; 版本号的唯一真源是 Cargo.toml；这里只做默认值，打包链通过 /DMyAppVersion= 覆盖。
; 默认值写死而不去解析 Cargo.toml，是因为 ISCC 的预处理没有文件读取能力，
; 而引入第三方脚本会让「编译安装器」多一个前置条件。
;
; The single source of truth for the version is Cargo.toml; this is only a default, and the
; packaging chain overrides it with /DMyAppVersion=. The default is hard-coded because the
; ISCC preprocessor cannot read files, and adding a third-party script would give "build the
; installer" one more prerequisite.
#ifndef MyAppVersion
  #define MyAppVersion "1.9.6"
#endif

[Setup]
; AppId 一经发布就不能改：它是「同一个程序」的身份，改了会变成并存的第二份安装。
; The AppId must never change after the first release: it is the identity of "this program",
; and changing it creates a second, coexisting installation.
AppId={{8F3C2A4E-5B71-4C9D-9A2E-1D6B7C0F3A55}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
VersionInfoVersion={#MyAppVersion}

; 默认装到 Program Files 下；普通用户可以用 /CURRENTUSER 装到自己目录（见下面的
; PrivilegesRequiredOverridesAllowed）。
; Defaults to Program Files; a non-admin user can pass /CURRENTUSER to install per-user.
DefaultDirName={autopf}\Aevum Studio
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
AllowNoIcons=yes
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=dialog commandline

; 只用 64 位；本项目的 ffmpeg.exe 与 ONNX 运行时都是 x64。
; 64-bit only: both the bundled ffmpeg.exe and the ONNX runtime are x64.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

OutputDir=..\..\dist\installer
OutputBaseFilename=Aevum-Studio-{#MyAppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayName={#MyAppName}
UninstallDisplayIcon={app}\{#MyAppExeName}

; 卸载器与安装器本身都要签名，否则 SmartScreen 只信 exe 不信套壳的那一层。
; 签名是可选步骤：scripts/sign.ps1 在没有证书时打印一行说明并 exit 0。
; Both the uninstaller and the setup itself get signed; otherwise SmartScreen trusts the exe
; but not the wrapper around it. Signing is optional: scripts/sign.ps1 prints one line and
; exits 0 when no certificate is configured.

; 语言：Inno 6 官方只带英文 Default.isl。中文界面需要社区翻译文件
; （jrsoftware/issrc 之外的 unofficial translations 里的 ChineseSimplified.isl）放到
; Inno 的 Languages 目录后再打开下面三行。**故意不默认开启**：文件不存在时 ISCC 会
; 直接编译失败，那会让「没有装翻译包的人」连英文安装包都打不出来。
;
; Language: Inno 6 ships only English (Default.isl). A Chinese UI needs the community
; translation (ChineseSimplified.isl from the unofficial translations) placed in Inno's
; Languages folder, then uncomment the three lines below. **Deliberately off by default**:
; a missing file makes ISCC fail outright, which would stop anyone without the translation
; package from building even the English installer.
;
; [Languages]
; Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
; Name: "english"; MessagesFile: "compiler:Default.isl"

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
; 桌面图标默认**不勾**：延时摄影师桌面上本来就堆满素材目录，多一个图标是噪音。
; 想放的人一次勾上就行。
; The desktop icon is **unchecked** by default: timelapse shooters already have their
; desktops full of footage folders, and one more icon is noise. One tick if you want it.
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; 整个 dist/aevum 原样装进去：aevum.exe、tools/ffmpeg.exe、licenses/、third-party-src/、
; README、THIRD-PARTY-NOTICES.txt 一个都不能少——LGPL-2.1 与 OFL 都要求随分发附带许可
; 文本与对应源码（见 THIRD-PARTY-NOTICES.txt 第 2 节）。漏装 licenses/ 不是「少个文件」，
; 是分发不合规。
;
; The whole dist/aevum tree goes in verbatim: aevum.exe, tools/ffmpeg.exe, licenses/,
; third-party-src/, README and THIRD-PARTY-NOTICES.txt. Nothing may be dropped -- both
; LGPL-2.1 and OFL require the licence texts and corresponding source to travel with the
; distribution (see section 2 of THIRD-PARTY-NOTICES.txt). Missing licenses/ is not "one
; fewer file", it is a non-compliant distribution.
Source: "..\..\dist\aevum\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\CHANGELOG.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
; 装完可选直接启动，默认勾上；/SILENT 下这一项照样执行（Inno 的 silent 不跑 [Run]，
; 除非带 postinstall nowait skipifsilent-unchecked —— 这里保持默认行为：静默安装不自动
; 起界面，批量部署的机器不该一装完就弹一个窗口）。
;
; Optionally launch after install, ticked by default. Under /SILENT this does not run: a
; machine being bulk-deployed should not pop a window the moment installation ends.
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; 安装目录里由程序自己生成的东西（崩溃日志、缓存预览）在卸载时一并清掉。
; Files the app itself writes into its own folder (crash logs, preview cache) go with it.
Type: filesandordirs; Name: "{app}\cache"
Type: filesandordirs; Name: "{app}\logs"

[Code]
// ---------------------------------------------------------------------------
// 卸载时问一句「配置要不要留」
//
// 为什么问而不直接删：配置目录（%APPDATA%\aevum-studio）里有配色、面板布局、
// 交付预设、插件——那是用户几年攒下来的东西，重装一次就没了是不可接受的损失。
// 为什么问而不直接留：有人就是要彻底清干净再也不用，留一个空目录等于骗他。
//
// 不删的默认答案是「留」：这个对话框只在用户明确选「否」时才动手。
//
// 静默卸载（/SILENT、/VERYSILENT）下没有人能回答，此时**一律保留**：批量部署的
// 脚本不该因为一个看不见的弹窗卡住，而「多留一个目录」是可恢复的，「误删配置」不是。
//
// ---------------------------------------------------------------------------
// Ask on uninstall whether to keep the configuration
//
// Why ask instead of always deleting: %APPDATA%\aevum-studio holds colour settings, panel
// layout, delivery presets and plugins -- years of the user's work, and losing them to a
// reinstall is unacceptable. Why ask instead of always keeping: some users do want a clean
// slate, and leaving an empty folder behind would be a lie.
//
// The default answer is "keep"; the deletion only happens on an explicit No.
//
// Under a silent uninstall (/SILENT, /VERYSILENT) nobody can answer, so the configuration is
// **always kept**: a bulk-deployment script must not hang on an invisible dialog, and "one
// leftover folder" is recoverable while "deleted settings" is not.

function ConfigDir(): String;
begin
  // 与 crates/aevum-core/src/config_dir.rs 的 dir() 同一路径：%APPDATA%\aevum-studio。
  // 旧版目录（lumen-studio / lapse-studio）由程序首启时自行迁移，卸载器不去碰。
  //
  // Same path as dir() in crates/aevum-core/src/config_dir.rs: %APPDATA%\aevum-studio.
  // The legacy folders (lumen-studio / lapse-studio) are migrated by the app on first
  // launch; the uninstaller leaves them alone.
  Result := ExpandConstant('{userappdata}\aevum-studio');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Dir: String;
begin
  if CurUninstallStep <> usPostUninstall then
    exit;

  Dir := ConfigDir();
  if not DirExists(Dir) then
    exit;

  // 静默卸载：没有人能回答，默认保留。
  // Silent uninstall: nobody can answer, so keep by default.
  if WizardSilent() then
  begin
    Log('Keeping user configuration (silent uninstall): ' + Dir);
    exit;
  end;

  if MsgBox('是否保留用户配置？' + #13#10 + #13#10 +
            Dir + #13#10 + #13#10 +
            '里面有配色、面板布局、交付预设与插件。' + #13#10 +
            '选「否」会连同这些一起删除，无法恢复。' + #13#10 + #13#10 +
            'Keep your settings? They live in the folder above and include colour ' +
            'settings, panel layout, delivery presets and plugins. Choosing No deletes ' +
            'them permanently.',
            mbConfirmation, MB_YESNO or MB_DEFBUTTON1) = IDNO then
  begin
    DelTree(Dir, True, True, True);
    Log('User configuration removed: ' + Dir);
  end
  else
    Log('User configuration kept: ' + Dir);
end;

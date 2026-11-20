# Drive a native Windows file dialog (IFileDialog: the folder picker of
# tauri-plugin-dialog) through UI Automation, the way a user would: wait for
# the dialog of the given process, type a path into its Folder/File name box
# and press its accept button, or press Cancel. Prints one JSON line with
# what it found and did. Used by e2e scenarios (scripts/e2e/native-dialog.mjs).
param(
  [Parameter(Mandatory = $true)] [string] $Title,
  [string] $Path = "",
  [switch] $Cancel,
  # A save dialog: type -Path as the file name (or keep the suggested name
  # when it is empty) and press Save. -Overwrite Yes/No answers the system's
  # "Confirm Save As" prompt (No, then Cancel: nothing is saved).
  [switch] $Save,
  [string] $Overwrite = "",
  [int] $ProcessId = 0,
  [int] $TimeoutMs = 20000
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$AE = [System.Windows.Automation.AutomationElement]
$Scope = [System.Windows.Automation.TreeScope]
$CT = [System.Windows.Automation.ControlType]

function Out-Result($result) {
  $result | ConvertTo-Json -Compress
}

function Find-Dialog {
  $candidates = @()
  $condition = [System.Windows.Automation.Condition]::TrueCondition
  if ($ProcessId -ne 0) {
    $condition = New-Object System.Windows.Automation.PropertyCondition($AE::ProcessIdProperty, $ProcessId)
  }
  foreach ($window in $AE::RootElement.FindAll($Scope::Children, $condition)) {
    $candidates += $window
    # A dialog can also be a child of the application window.
    foreach ($child in $window.FindAll($Scope::Children, [System.Windows.Automation.Condition]::TrueCondition)) {
      $candidates += $child
    }
  }
  foreach ($element in $candidates) {
    try {
      if ($element.Current.ClassName -eq "#32770" -and $element.Current.Name -eq $Title) { return $element }
    } catch {}
  }
  return $null
}


function Is-Open($dialog) {
  try { $null = $dialog.Current.Name; return $true } catch { return $false }
}

function Wait-Closed($dialog, [int] $ms) {
  $until = (Get-Date).AddMilliseconds($ms)
  while ((Get-Date) -lt $until) {
    if (-not (Is-Open $dialog)) { return $true }
    if ($null -eq (Find-Dialog)) { return $true }
    Start-Sleep -Milliseconds 150
  }
  return $false
}

$deadline = (Get-Date).AddMilliseconds($TimeoutMs)
$dialog = $null
while ($null -eq $dialog -and (Get-Date) -lt $deadline) {
  $dialog = Find-Dialog
  if ($null -eq $dialog) { Start-Sleep -Milliseconds 200 }
}
if ($null -eq $dialog) {
  Out-Result @{ found = $false; title = $Title }
  exit 2
}

$result = @{ found = $true; title = $Title; className = $dialog.Current.ClassName; processId = $dialog.Current.ProcessId }

# Controls are found by their dialog control id (AutomationId): the classic
# controls of the dialog (path box 1152/1148, accept button 1, Cancel 2) can
# appear to UI Automation as plain window panes without Value/Invoke patterns
# or focus. Then the driver uses those controls' own window messages, as the
# keyboard and mouse would: WM_SETTEXT into the path box and the button's
# WM_COMMAND (BN_CLICKED) to the dialog.
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class XpDialogInput {
  [DllImport("user32.dll", CharSet = CharSet.Unicode)]
  public static extern IntPtr SendMessage(IntPtr hWnd, uint msg, IntPtr wParam, string lParam);
  [DllImport("user32.dll")]
  public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
  public const uint WM_SETTEXT = 0x000C;
  public const uint WM_COMMAND = 0x0111;
  public static void SetText(IntPtr box, string text) { SendMessage(box, WM_SETTEXT, IntPtr.Zero, text); }
  [DllImport("user32.dll", EntryPoint = "SendMessageW")]
  public static extern IntPtr SendMessageRaw(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
  public const uint EM_SETSEL = 0x00B1;
  public const uint EM_REPLACESEL = 0x00C2;
  // Replace an Edit's whole text the way typing does (the edit is marked
  // modified and notifies its dialog; WM_SETTEXT alone does neither, and a
  // save dialog then keeps the name it suggested).
  public const uint TDM_CLICK_BUTTON = 0x0466;
  public static void ClickTaskButton(IntPtr taskDialog, int id) {
    PostMessage(taskDialog, TDM_CLICK_BUTTON, new IntPtr(id), IntPtr.Zero);
  }
  public static void TypeText(IntPtr box, string text) {
    SendMessageRaw(box, EM_SETSEL, IntPtr.Zero, new IntPtr(-1));
    SendMessage(box, EM_REPLACESEL, new IntPtr(1), text);
  }
  public static void Click(IntPtr dialog, int id, IntPtr button) {
    // MAKEWPARAM(id, BN_CLICKED = 0)
    PostMessage(dialog, WM_COMMAND, new IntPtr(id & 0xFFFF), button);
  }
}
"@

# Only the dialog's classic controls: list items carry their index as an
# AutomationId too ("1" is the second folder in the list, not the button).
$ClassicClasses = @("Button", "Edit", "ComboBox", "ComboBoxEx32", "Static", "ToolbarWindow32")
function Find-Id($dialog, [string] $automationId) {
  $found = $dialog.FindAll($Scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition($AE::AutomationIdProperty, $automationId)))
  foreach ($element in $found) {
    if ($ClassicClasses -contains $element.Current.ClassName) { return $element }
  }
  return $null
}

function Wait-Id($dialog, [string[]] $ids, [int] $ms) {
  $until = (Get-Date).AddMilliseconds($ms)
  while ((Get-Date) -lt $until) {
    foreach ($id in $ids) {
      $element = Find-Id $dialog $id
      if ($element) { return $element }
    }
    Start-Sleep -Milliseconds 250
  }
  return $null
}

function Handle($element) {
  return [IntPtr]$element.Current.NativeWindowHandle
}

function Press($dialog, $button, [int] $id) {
  try {
    $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    return "InvokePattern"
  } catch {
    [XpDialogInput]::Click((Handle $dialog), $id, (Handle $button))
    return "WM_COMMAND"
  }
}

# A save dialog's "File name:" box: the Edit with control id 1001 (the
# address bar has the same id and another class). Its name is its text.
function Find-SaveBox {
  $condition = New-Object System.Windows.Automation.AndCondition(
    (New-Object System.Windows.Automation.PropertyCondition($AE::AutomationIdProperty, "1001")),
    (New-Object System.Windows.Automation.PropertyCondition($AE::ClassNameProperty, "Edit"))
  )
  $until = (Get-Date).AddMilliseconds(8000)
  while ((Get-Date) -lt $until) {
    $edit = $dialog.FindFirst($Scope::Descendants, $condition)
    if ($edit) { return $edit }
    Start-Sleep -Milliseconds 250
  }
  return $null
}
$saveBox = $null
if ($Save) {
  $saveBox = Find-SaveBox
  if ($null -eq $saveBox) {
    $result.error = "no file name box"
    $all = $dialog.FindAll($Scope::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    $result.controls = @($all | ForEach-Object { "$($_.Current.ControlType.ProgrammaticName)|$($_.Current.AutomationId)|$($_.Current.Name)|$($_.Current.ClassName)" })
    Out-Result $result
    exit 3
  }
  try { $result.suggested = $saveBox.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value } catch { $result.suggested = $saveBox.Current.Name }
  $address = $dialog.FindFirst($Scope::Descendants, (New-Object System.Windows.Automation.AndCondition(
    (New-Object System.Windows.Automation.PropertyCondition($AE::AutomationIdProperty, "1001")),
    (New-Object System.Windows.Automation.PropertyCondition($AE::ClassNameProperty, "ToolbarWindow32"))
  )))
  if ($address) { $result.folder = ($address.Current.Name -replace '^Address:\s*', '') }
}

if ($Cancel) {
  $button = Wait-Id $dialog @("2") 8000
  if ($null -eq $button) {
    $result.error = "no Cancel button"
    Out-Result $result
    exit 3
  }
  $result.cancelButton = $button.Current.Name
  $result.input = Press $dialog $button 2
  $result.action = "cancel"
  $result.closed = Wait-Closed $dialog 5000
  Out-Result $result
  if (-not $result.closed) { exit 4 }
  exit 0
}

if ($Save) {
  $steps = @()
  if ($Path -ne "") {
    [XpDialogInput]::TypeText((Handle $saveBox), $Path)
    Start-Sleep -Milliseconds 300
    $steps += "type: EM_REPLACESEL"
    $result.typed = $Path
  }
  $accept = Wait-Id $dialog @("1") 8000
  if ($null -eq $accept) {
    $result.error = "no Save button"
    Out-Result $result
    exit 3
  }
  $result.acceptButton = $accept.Current.Name
  $steps += "accept: $(Press $dialog $accept 1)"
  # Either the dialog closes, or the system asks before replacing a file
  # (a "Confirm Save As" window owned by the dialog, with Yes and No).
  $result.overwritePrompt = $false
  $promptCondition = New-Object System.Windows.Automation.AndCondition(
    (New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, $CT::Window)),
    (New-Object System.Windows.Automation.PropertyCondition($AE::NameProperty, "Confirm Save As"))
  )
  $until = (Get-Date).AddMilliseconds(8000)
  $prompt = $null
  while ((Get-Date) -lt $until -and (Is-Open $dialog) -and ($null -ne (Find-Dialog))) {
    try { $prompt = $dialog.FindFirst($Scope::Descendants, $promptCondition) } catch { $prompt = $null }
    if ($prompt) { break }
    Start-Sleep -Milliseconds 200
  }
  if ($prompt) {
    $result.overwritePrompt = $true
    try {
      $texts = $prompt.FindAll($Scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, $CT::Text)))
      $result.promptText = (@($texts | ForEach-Object { $_.Current.Name }) -join " ")
    } catch {}
    if ($Overwrite -eq "") {
      $result.error = "the system asked to replace a file and no -Overwrite answer was given"
      $Overwrite = "No"
    }
    # The prompt is a task dialog: its buttons (CommandButton_6 Yes,
    # CommandButton_7 No) have no Invoke pattern here, so it is asked to
    # click one itself (TDM_CLICK_BUTTON with IDYES 6 / IDNO 7).
    $answerId = if ($Overwrite -eq "Yes") { 6 } else { 7 }
    $answer = $prompt.FindFirst($Scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition($AE::AutomationIdProperty, "CommandButton_$answerId")))
    if ($null -eq $answer) {
      $result.error = "no $Overwrite button in the overwrite prompt"
      $all = $prompt.FindAll($Scope::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
      $result.controls = @($all | ForEach-Object { "$($_.Current.ControlType.ProgrammaticName)|$($_.Current.AutomationId)|$($_.Current.Name)|$($_.Current.ClassName)" })
      $result.steps = $steps
      Out-Result $result
      exit 3
    }
    $result.answerButton = $answer.Current.Name
    try {
      $answer.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
      $steps += "answer: InvokePattern"
    } catch {
      [XpDialogInput]::ClickTaskButton((Handle $prompt), $answerId)
      $steps += "answer: TDM_CLICK_BUTTON"
    }
    $steps += "overwrite: $Overwrite"
    if ($Overwrite -ne "Yes") {
      # Back in the save dialog: leave it without saving.
      Start-Sleep -Milliseconds 400
      $cancelButton = Wait-Id $dialog @("2") 8000
      if ($cancelButton) { $steps += "cancel: $(Press $dialog $cancelButton 2)" }
    }
  } elseif ($Overwrite -ne "") {
    $result.error = "the system did not ask before replacing"
  }
  $result.steps = $steps
  $result.action = "save"
  $result.closed = Wait-Closed $dialog 8000
  if (-not $result.closed) {
    # Something else is in the way (a message of the dialog): leave it.
    $cancelButton = Find-Id $dialog "2"
    if ($cancelButton) { $null = Press $dialog $cancelButton 2 }
  }
  Out-Result $result
  if (-not $result.closed) { exit 4 }
  if ($result.error) { exit 6 }
  exit 0
}

# The path box: "Folder:" in a folder picker (1152), "File name:" otherwise
# (1148, an Edit inside a ComboBox). The dialog's controls appear a moment
# after its window, so look for a few seconds.
$box = Wait-Id $dialog @("1152", "1148") 8000
if ($null -eq $box) {
  $result.error = "no path box"
  $all = $dialog.FindAll($Scope::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
  $result.controls = @($all | ForEach-Object { "$($_.Current.ControlType.ProgrammaticName)|$($_.Current.AutomationId)|$($_.Current.Name)|$($_.Current.ClassName)" })
  Out-Result $result
  exit 3
}
$inner = $box.FindFirst($Scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition($AE::ClassNameProperty, "Edit")))
if ($inner) { $box = $inner }
$label = Find-Id $dialog "1090"
$result.pathBox = if ($label) { $label.Current.Name } else { $box.Current.AutomationId }

function Set-Box([string] $text) {
  try {
    $box.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($text)
    return "ValuePattern"
  } catch {
    [XpDialogInput]::SetText((Handle $box), $text)
    return "WM_SETTEXT"
  }
}

function Accept {
  $accept = Find-Id $dialog "1"
  if ($null -eq $accept) { throw "no accept button" }
  $result.acceptButton = $accept.Current.Name
  return (Press $dialog $accept 1)
}

# As a user does it: go to the parent folder (typing its path, with a
# trailing separator, navigates there), select the folder in the list, and
# press the accept button. Typing the folder's own path would make a folder
# picker navigate into it instead of choosing it.
$target = $Path.TrimEnd("\", "/")
$parent = Split-Path -Parent $target
$leaf = Split-Path -Leaf $target
$steps = @()
$steps += "type parent: $(Set-Box ($parent + '\'))"
$steps += "accept: $(Accept)"
# Wait until the dialog shows the parent folder (its address bar names it),
# so the folder selected below is the one inside it.
$until = (Get-Date).AddMilliseconds(10000)
$arrived = $false
while (-not $arrived -and (Get-Date) -lt $until -and (Is-Open $dialog)) {
  $address = Find-Id $dialog "1001"
  if ($address -and ($address.Current.Name -replace '^Address:\s*', '').TrimEnd("\") -ieq $parent) { $arrived = $true }
  else { Start-Sleep -Milliseconds 250 }
}
if (-not $arrived) {
  $result.error = "the dialog did not go to the parent folder"
  $result.steps = $steps
  $address = Find-Id $dialog "1001"
  $result.address = if ($address) { $address.Current.Name } else { $null }
  Out-Result $result
  exit 5
}
$steps += "at: $parent"
$item = $null
$until = (Get-Date).AddMilliseconds(10000)
$itemCondition = New-Object System.Windows.Automation.AndCondition(
  (New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, $CT::ListItem)),
  (New-Object System.Windows.Automation.PropertyCondition($AE::NameProperty, $leaf))
)
while ($null -eq $item -and (Get-Date) -lt $until -and (Is-Open $dialog)) {
  $item = $dialog.FindFirst($Scope::Descendants, $itemCondition)
  if ($null -eq $item) {
    # A long folder list is virtualized: ask the list for the item and realize it.
    $list = $dialog.FindFirst($Scope::Descendants, (New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, $CT::List)))
    if ($list) {
      try {
        $container = $list.GetCurrentPattern([System.Windows.Automation.ItemContainerPattern]::Pattern)
        $virtual = $container.FindItemByProperty($null, $AE::NameProperty, $leaf)
        if ($virtual) {
          try { $virtual.GetCurrentPattern([System.Windows.Automation.VirtualizedItemPattern]::Pattern).Realize() } catch {}
          $item = $virtual
          $steps += "realized: $leaf"
        }
      } catch {}
    }
  }
  if ($null -eq $item) { Start-Sleep -Milliseconds 250 }
}
if ($null -eq $item) {
  $result.error = "the folder was not listed after going to its parent"
  $result.steps = $steps
  $address = Find-Id $dialog "1001"
  $result.address = if ($address) { $address.Current.Name } else { $null }
  Out-Result $result
  exit 5
}
$item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
$steps += "select item: $leaf"
Start-Sleep -Milliseconds 300
$steps += "accept: $(Accept)"
$result.typed = $Path
$result.steps = $steps
$result.closed = Wait-Closed $dialog 5000
Out-Result $result
if (-not $result.closed) { exit 4 }
exit 0

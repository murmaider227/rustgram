# Notification-area (tray) icon of a client, through UI Automation: no real mouse moves.
#   -Action find   saves a screen capture of the icon to -Out
#   -Action click  invokes the icon (what a left click does)
# Prints "OK ..." on success. Icons Windows 11 keeps in the overflow flyout are reached by
# invoking the "Show hidden icons" button first (the flyout opens briefly and is closed again).
param(
    [Parameter(Mandatory = $true)][string]$Name,
    [Parameter(Mandatory = $true)][string]$Action,
    [string]$Out = ""
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
$root = [System.Windows.Automation.AutomationElement]::RootElement
$tree = [System.Windows.Automation.TreeScope]::Descendants

function Find-Icon {
    $cond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button)
    foreach ($cls in @("Shell_TrayWnd", "TopLevelWindowForOverflowXamlIsland", "NotifyIconOverflowWindow")) {
        $wc = New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::ClassNameProperty, $cls)
        $win = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $wc)
        if ($null -eq $win) { continue }
        foreach ($b in $win.FindAll($tree, $cond)) {
            $n = $b.Current.Name
            if ($n -and $n.StartsWith($Name) -and -not $n.StartsWith("$Name Desktop")) { return $b }
        }
    }
    return $null
}

function Find-Chevron {
    $wc = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ClassNameProperty, "Shell_TrayWnd")
    $tray = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $wc)
    if ($null -eq $tray) { return $null }
    foreach ($b in $tray.FindAll($tree, [System.Windows.Automation.Condition]::TrueCondition)) {
        $aid = $b.Current.AutomationId
        $n = $b.Current.Name
        if ($aid -eq "SystemTrayIcon" -and $n -match "hidden|Hidden") { return $b }
        if ($n -match "^Show hidden icons|^Show Hidden Icons") { return $b }
    }
    return $null
}

$icon = Find-Icon
$opened = $false
if ($null -eq $icon) {
    $chev = Find-Chevron
    if ($null -ne $chev) {
        $chev.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 800
        $opened = $true
        $icon = Find-Icon
    }
}
if ($null -eq $icon) { Write-Output "NOT_FOUND no tray icon named $Name"; exit 3 }

$r = $icon.Current.BoundingRectangle
if ($Action -eq "find") {
    if ($Out -ne "" -and $r.Width -gt 0) {
        $bmp = New-Object System.Drawing.Bitmap ([int]$r.Width), ([int]$r.Height)
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen([int]$r.X, [int]$r.Y, 0, 0, $bmp.Size)
        $g.Dispose()
        $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
    }
    Write-Output ("OK found {0}x{1}" -f [int]$r.Width, [int]$r.Height)
} elseif ($Action -eq "click") {
    $icon.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Write-Output "OK clicked"
} else {
    Write-Output "UNKNOWN_ACTION"; exit 2
}
if ($opened -and $Action -eq "find") {
    # Close the overflow flyout again.
    $chev = Find-Chevron
    if ($null -ne $chev) { $chev.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
}

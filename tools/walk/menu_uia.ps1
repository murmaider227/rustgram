# Invokes an item of an open Win32 popup menu (class #32768) owned by a process, through UI
# Automation: no real mouse or keyboard input. Prints "OK <item>" or the reason it could not.
#   powershell -File menu_uia.ps1 -ProcId <pid> -Item "Quit Rustgram"
param(
    [Parameter(Mandatory = $true)][int]$ProcId,
    [Parameter(Mandatory = $true)][string]$Item
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$root = [System.Windows.Automation.AutomationElement]::RootElement
$cls = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ClassNameProperty, "#32768")
$pidc = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $ProcId)
$both = New-Object System.Windows.Automation.AndCondition($cls, $pidc)
$menu = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $both)
if ($null -eq $menu) { Write-Output "NO_MENU"; exit 3 }
$items = $menu.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
foreach ($it in $items) {
    if ($it.Current.Name -eq $Item) {
        $it.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Write-Output "OK $Item"
        exit 0
    }
}
Write-Output ("NO_ITEM; items: " + (($items | % { $_.Current.Name }) -join " | "))
exit 4

# Windows OCR (Windows.Media.Ocr) for the scenario walk: reads PNG files and prints one JSON
# object per line: {"image": <path>, "lines": [{"text", "x", "y", "w", "h"}]} in image pixels.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File ocr.ps1 -Lang en-US -Images a.png b.png
param(
    [string]$Lang = "en-US",
    [switch]$Serve,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Images
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$null = [Windows.Storage.StorageFile, Windows.Storage, ContentType = WindowsRuntime]
$null = [Windows.Media.Ocr.OcrEngine, Windows.Foundation, ContentType = WindowsRuntime]
$null = [Windows.Globalization.Language, Windows.Globalization, ContentType = WindowsRuntime]
$null = [Windows.Graphics.Imaging.BitmapDecoder, Windows.Graphics, ContentType = WindowsRuntime]
$null = [Windows.Graphics.Imaging.SoftwareBitmap, Windows.Graphics, ContentType = WindowsRuntime]

$asTaskGeneric = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
        $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
        $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
    })[0]
function Wait-Op($op, [Type]$type) {
    $task = $asTaskGeneric.MakeGenericMethod($type).Invoke($null, @($op))
    $task.Wait() | Out-Null
    return $task.Result
}

$language = New-Object Windows.Globalization.Language $Lang
$engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromLanguage($language)
if ($null -eq $engine) { throw "OCR language $Lang is not installed" }

function Read-Image([string]$img) {
    $full = (Resolve-Path $img).Path
    $file = Wait-Op ([Windows.Storage.StorageFile]::GetFileFromPathAsync($full)) ([Windows.Storage.StorageFile])
    $stream = Wait-Op ($file.OpenAsync([Windows.Storage.FileAccessMode]::Read)) ([Windows.Storage.Streams.IRandomAccessStream])
    $decoder = Wait-Op ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream)) ([Windows.Graphics.Imaging.BitmapDecoder])
    $bitmap = Wait-Op ($decoder.GetSoftwareBitmapAsync()) ([Windows.Graphics.Imaging.SoftwareBitmap])
    $result = Wait-Op ($engine.RecognizeAsync($bitmap)) ([Windows.Media.Ocr.OcrResult])
    $lines = @()
    foreach ($line in $result.Lines) {
        $x0 = 1e9; $y0 = 1e9; $x1 = 0; $y1 = 0
        foreach ($w in $line.Words) {
            $r = $w.BoundingRect
            $x0 = [Math]::Min($x0, $r.X); $y0 = [Math]::Min($y0, $r.Y)
            $x1 = [Math]::Max($x1, $r.X + $r.Width); $y1 = [Math]::Max($y1, $r.Y + $r.Height)
        }
        $lines += [pscustomobject]@{ text = $line.Text; x = [int]$x0; y = [int]$y0; w = [int]($x1 - $x0); h = [int]($y1 - $y0) }
    }
    $bitmap.Dispose()
    $stream.Dispose()
    [pscustomobject]@{ image = $full; lines = $lines } | ConvertTo-Json -Depth 4 -Compress
}

if ($Serve) {
    # Server mode: one image path per stdin line, one JSON line back (keeps WinRT loaded).
    while ($true) {
        $line = [Console]::In.ReadLine()
        if ($null -eq $line -or $line -eq "") { break }
        try { Read-Image $line } catch { [pscustomobject]@{ image = $line; lines = @(); error = "$_" } | ConvertTo-Json -Compress }
        [Console]::Out.Flush()
    }
} else {
    foreach ($img in $Images) { Read-Image $img }
}

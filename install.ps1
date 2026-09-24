# Installs the latest rtools release on Windows:
#
#   irm https://raw.githubusercontent.com/ssenerg/rtools/main/install.ps1 | iex
#
# $env:RTOOLS_VERSION picks a version (default: the latest release) and
# $env:RTOOLS_INSTALL_DIR where it goes (default: %LOCALAPPDATA%\Programs\rtools).

# A script block keeps these variables out of your session, and an error only
# stops the install instead of closing your terminal.
& {
    $ErrorActionPreference = 'Stop'
    # Windows PowerShell downloads far slower while it draws a progress bar.
    $ProgressPreference = 'SilentlyContinue'
    # Older Windows PowerShell setups don't offer TLS 1.2, which GitHub requires.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    function Save-Url($url, $path) {
        try {
            Invoke-WebRequest $url -OutFile $path -UseBasicParsing
        } catch {
            throw "Couldn't download ${url}: $($_.Exception.Message)"
        }
    }

    $repo = 'ssenerg/rtools'
    $target = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
    $dir = if ($env:RTOOLS_INSTALL_DIR) { $env:RTOOLS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\rtools' }

    $version = $env:RTOOLS_VERSION
    if (-not $version) {
        # The latest-release link redirects to the release's tag.
        $response = Invoke-WebRequest "https://github.com/$repo/releases/latest" -Method Head -UseBasicParsing
        $final = if ($response.BaseResponse.ResponseUri) {
            $response.BaseResponse.ResponseUri.AbsoluteUri                  # Windows PowerShell 5.1
        } else {
            $response.BaseResponse.RequestMessage.RequestUri.AbsoluteUri    # PowerShell 7
        }
        if ($final -notmatch '/tag/') { throw 'No rtools release has been published yet.' }
        $version = ($final -split '/tag/')[-1]
    }
    $version = $version.TrimStart('v')

    $archive = "rtools-$version-$target.zip"
    $base = "https://github.com/$repo/releases/download/v$version"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) "rtools-install-$([guid]::NewGuid())"
    New-Item -ItemType Directory $tmp | Out-Null
    try {
        Write-Host "Downloading rtools $version for $target..."
        $zip = Join-Path $tmp $archive
        Save-Url "$base/$archive" $zip
        $sums = Join-Path $tmp 'SHA256SUMS'
        Save-Url "$base/SHA256SUMS" $sums

        $line = Get-Content $sums | Where-Object { ($_ -split '\s+')[-1].TrimStart('*') -eq $archive } | Select-Object -First 1
        if (-not $line) { throw "SHA256SUMS lists no checksum for $archive." }
        $expected = ($line -split '\s+')[0]
        $actual = (Get-FileHash $zip -Algorithm SHA256).Hash
        if ($actual -ne $expected) { throw "$archive doesn't match its checksum, not installing it." }

        Expand-Archive $zip $tmp -Force
        New-Item -ItemType Directory -Force $dir | Out-Null
        Move-Item (Join-Path $tmp "rtools-$version-$target\rtools.exe") (Join-Path $dir 'rtools.exe') -Force
        Write-Host "Installed rtools $version to $(Join-Path $dir 'rtools.exe')"

        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        if ((($userPath -split ';') | Where-Object { $_ }) -notcontains $dir) {
            $newPath = (@($userPath -split ';' | Where-Object { $_ }) + $dir) -join ';'
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            $env:Path = "$env:Path;$dir"
            Write-Host "Added $dir to your PATH. Open a new terminal to use rtools everywhere."
        }
    } finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

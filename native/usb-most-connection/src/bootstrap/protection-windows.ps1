# Child Windows PowerShell must use its own trusted modules, not inherited pwsh/user paths.
$env:PSModulePath = [IO.Path]::Combine($PSHOME, 'Modules')
function Protected([string]$path, [bool]$directory) {
    $script:protectionFailure = 'missing'
    try {
    if (!(Test-Path -LiteralPath $path)) { return $false }
    $script:protectionFailure = 'type'
    $item = Get-Item -LiteralPath $path -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or $item.PSIsContainer -ne $directory) { return $false }
    $script:protectionFailure = 'owner'
    $acl = if ($directory) { [IO.Directory]::GetAccessControl($path) } else { [IO.File]::GetAccessControl($path) }
    $owner = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
    if ($owner -notin @('S-1-5-18', 'S-1-5-32-544')) { return $false }
    $script:protectionFailure = 'inherited'
    if ($directory -and !$acl.AreAccessRulesProtected) { return $false }
    $script:protectionFailure = 'count'
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    if ($rules.Count -ne 2) { return $false }
    $ids = @()
    foreach ($rule in $rules) {
        $script:protectionFailure = 'rights'
        if ($rule.AccessControlType -ne 'Allow' -or $rule.FileSystemRights -ne [Security.AccessControl.FileSystemRights]::FullControl -or $rule.PropagationFlags -ne 'None') { return $false }
        $script:protectionFailure = 'flags'
        if ($directory -and $rule.InheritanceFlags -ne ([Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit)) { return $false }
        $ids += $rule.IdentityReference.Value
    }
    $script:protectionFailure = 'principals'
    return ('S-1-5-18' -in $ids -and 'S-1-5-32-544' -in $ids)
    } catch { $script:protectionFailure = 'exception-' + $_.Exception.GetType().Name; return $false }
}

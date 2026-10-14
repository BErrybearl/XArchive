# Shared by history and public-tree scanning.
function Get-SensitiveRules {
@(
    @{ Name = 'X 会话凭据'; Secret = $true; Pattern = '(?i)\b(?:auth_token|ct0)["'']?\s*[=:]\s*["'']?(?<value>[0-9a-f]{32,})\b' },
    @{ Name = 'Bearer token'; Secret = $true; Pattern = '(?i:\bBearer\s+)(?<value>[A-Za-z0-9_%+./=-]{24,})|(?<value>AAAAA[A-Za-z0-9_%+./=-]{25,})' },
    @{ Name = '常见密钥'; Secret = $true; Pattern = '(?<value>-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----|\bgh[pousr]_[A-Za-z0-9]{16,}\b|\bgithub_pat_[A-Za-z0-9_]{20,}\b|\bsk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{16,}\b|\bAKIA[A-Z0-9]{16}\b)' },
    @{ Name = 'Cookie 头'; Secret = $true; Pattern = '(?i)(?:\b(?:set-cookie|cookie)\s*:\s*|["''](?:set-cookie|cookie)["'']\s*[:,=]\s*)(?<value>[^\r\n]+)' },
    @{ Name = '邮箱地址'; Secret = $false; Pattern = '(?i)(?<value>[A-Z0-9.!#$%&''*+/=?^_`{|}~-]+@[A-Z0-9](?:[A-Z0-9.-]*[A-Z0-9])?\.[A-Z]{2,})' },
    @{ Name = 'Windows 用户路径'; Secret = $false; Pattern = '(?i)(?<value>C:[\\/]+Users[\\/]+[^\\/\s"''<>;`]+)' },
    @{ Name = '未列入公开白名单的 GitHub 链接'; Secret = $false; Pattern = '(?i)github\.com[/:](?<value>[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)' },
    @{ Name = 'IP:端口代理地址'; Secret = $false; Pattern = '(?<![\d.])(?<value>(?:\d{1,3}\.){3}\d{1,3}:\d{2,5}|\[(?:[0-9A-Fa-f:]+)\]:\d{2,5})(?!\d)' }
)
}

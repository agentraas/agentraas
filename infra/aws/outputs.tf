output "url" {
  description = "Where AgentRaaS answers: the dashboard is at /dashboard."
  value       = local.public_url
}

output "load_balancer_dns_name" {
  description = "Point your domain's CNAME here when you use public_url and a certificate."
  value       = aws_lb.this.dns_name
}

output "kms_key_arn" {
  description = "Holds the credentials master key. Never schedule it for deletion: without it every stored service key is lost."
  value       = aws_kms_key.this.arn
}

output "task_role_name" {
  description = "Grant this role read access to the secrets your Enterprise secret references (aws-sm://) name."
  value       = aws_iam_role.task.name
}

output "logs" {
  value = "aws logs tail ${aws_cloudwatch_log_group.api.name} --follow --region ${var.region}"
}

output "instance_public_ip" {
  description = "Public IPv4 for SSH and (after TLS) Slack Events URL."
  value       = module.aws_stack.instance_public_ip
}

output "instance_id" {
  value = module.aws_stack.instance_id
}

output "vpc_id" {
  value = module.aws_stack.vpc_id
}

output "ssh_command" {
  description = "Default user is ubuntu on Canonical images."
  value       = module.aws_stack.ssh_command
}

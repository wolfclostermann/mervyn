output "instance_public_ip" {
  description = "Public IPv4, for SSH. Mervyn exposes no public endpoint: it reaches Telegram by outbound long polling."
  value       = aws_instance.mervyn.public_ip
}

output "instance_id" {
  value = aws_instance.mervyn.id
}

output "vpc_id" {
  value = aws_vpc.this.id
}

output "ssh_command" {
  description = "Default user is ubuntu on Canonical images."
  value       = "ssh ubuntu@${aws_instance.mervyn.public_ip}"
}

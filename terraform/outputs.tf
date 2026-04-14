output "instance_public_ip" {
  description = "Public IPv4 for SSH and (after TLS) Slack Events URL."
  value       = oci_core_instance.mervyn.public_ip
}

output "instance_ocid" {
  value = oci_core_instance.mervyn.id
}

output "vcn_id" {
  value = oci_core_vcn.this.id
}

output "ssh_command" {
  description = "Default user is ubuntu on Canonical images."
  value       = "ssh ubuntu@${oci_core_instance.mervyn.public_ip}"
}

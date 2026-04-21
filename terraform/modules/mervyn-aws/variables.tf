variable "project_name" {
  type        = string
  description = "Prefix for resource names and tags."
  default     = "mervyn"
}

variable "vpc_cidr_block" {
  type        = string
  description = "VPC IPv4 CIDR."
  default     = "10.0.0.0/16"
}

variable "public_subnet_cidr_block" {
  type        = string
  description = "Public subnet CIDR (must fit inside vpc_cidr_block)."
  default     = "10.0.1.0/24"
}

variable "instance_type" {
  type        = string
  description = "EC2 instance type (x86_64 Ubuntu AMI below)."
  default     = "t3.micro"
}

variable "ssh_public_key" {
  type        = string
  description = "SSH public key material for aws_key_pair."
}

variable "ssh_allowed_cidrs" {
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "http_cidrs" {
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "expose_app_port" {
  type    = bool
  default = false
}

variable "app_port_cidrs" {
  type    = list(string)
  default = ["0.0.0.0/0"]
}

variable "bootstrap_docker" {
  type    = bool
  default = true
}

variable "cloud_init_file" {
  type = string
}

variable "ubuntu_version" {
  type        = string
  description = "Ubuntu LTS major.minor for AMI filter (22.04)."
  default     = "22.04"
}

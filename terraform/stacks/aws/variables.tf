variable "aws_region" {
  type        = string
  description = "AWS region, e.g. eu-west-2, us-east-1."
  default     = "eu-west-2"
}

variable "project_name" {
  type    = string
  default = "mervyn"
}

variable "vpc_cidr_block" {
  type    = string
  default = "10.0.0.0/16"
}

variable "public_subnet_cidr_block" {
  type    = string
  default = "10.0.1.0/24"
}

variable "instance_type" {
  type    = string
  default = "t3.micro"
}

variable "ssh_public_key" {
  type = string
}

variable "ssh_allowed_cidrs" {
  type    = list(string)
  default = ["0.0.0.0/0"]
}

variable "http_cidrs" {
  type    = list(string)
  default = ["0.0.0.0/0"]
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

variable "ubuntu_version" {
  type    = string
  default = "22.04"
}

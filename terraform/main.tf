# Stack composition follows patterns from the GFS Terraform framework example:
# https://github.com/gfs-sre/terraform-framework-example-stack
# (Enterprise SSO repo — mirror internally or align module boundaries with that layout.)

module "oci_stack" {
  source = "./modules/mervyn-oci"

  tenancy_ocid     = var.tenancy_ocid
  user_ocid        = var.user_ocid
  api_fingerprint  = var.api_fingerprint
  private_key_path = var.private_key_path
  region           = var.region
  compartment_ocid = var.compartment_ocid

  project_name      = var.project_name
  ssh_public_key    = var.ssh_public_key
  ssh_allowed_cidrs = var.ssh_allowed_cidrs
  expose_app_port   = var.expose_app_port
  app_port_cidrs    = var.app_port_cidrs
  http_cidrs        = var.http_cidrs

  instance_shape            = var.instance_shape
  instance_ocpus            = var.instance_ocpus
  instance_memory_gbs       = var.instance_memory_gbs
  instance_source_image_id  = var.instance_source_image_id
  instance_display_name     = var.instance_display_name
  ssh_user                  = var.ssh_user
  availability_domain_index = var.availability_domain_index
  ubuntu_version            = var.ubuntu_version
  bootstrap_docker          = var.bootstrap_docker

  cloud_init_file = "${path.root}/cloud-init-docker.yaml"
}
